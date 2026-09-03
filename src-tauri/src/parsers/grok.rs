use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use sha2::{Digest, Sha256};

use crate::commands::confined_file::has_dangling_alias_component;
use crate::models::message::AutonomousTurnOrigin;
use crate::models::{
    AgentType, ContentBlock, ConversationDetail, ConversationSummary, MessageTurn, TurnRole,
    TurnUsage,
};
use crate::parsers::{
    backfill_turn_durations, compute_session_stats, folder_name_from_path,
    infer_context_window_max_tokens, latest_turn_total_usage_tokens, merge_context_window_stats,
    relocate_orphaned_tool_results, structurize_read_tool_output, title_from_user_text,
    truncate_str, visible_title, visible_user_text, AgentParser, ParseError,
};

/// Cap for a single tool result / tool input preview stored on a turn. Grok's
/// `tool_call_update.content` is **cumulative** (each update carries the whole
/// output so far), and long-running commands can emit tens of KB — bound it so
/// a single noisy command can't bloat a conversation detail payload.
const GROK_TOOL_OUTPUT_CAP: usize = 100_000;
const GROK_TOOL_INPUT_CAP: usize = 8_000;

/// Budget for a serialized `TaskOutput` envelope (see `grok_task_output_envelope`).
/// Deliberately below the live path's `MAX_SINGLE_EMIT_BYTES` (64 KiB, see
/// `acp::connection`): the envelope is JSON the frontend parses, and the live
/// emitter truncates from the head with a marker — which would corrupt it. Both
/// paths share this function, so a background command's output is capped here
/// rather than shredded downstream.
const GROK_TASK_OUTPUT_CAP: usize = 48 * 1024;

/// Tool name the parser assigns to grok's native `ask_user_question` (from its
/// `_meta["x.ai/tool"].name`). Used to find the ask ToolResults whose answer must
/// be recovered from `chat_history.jsonl` (see `inject_grok_ask_answers`).
const GROK_ASK_TOOL_NAME: &str = "ask_user_question";

/// Resolve Grok's data home, honoring `GROK_HOME`, else `~/.grok` (mirrors the
/// CLI's own `GROK_HOME` override). The transcript store lives under the
/// `sessions/` subdirectory of this path.
pub(crate) fn resolve_grok_home_dir() -> PathBuf {
    resolve_grok_home_from(std::env::var_os("GROK_HOME"), dirs::home_dir())
}

fn resolve_grok_home_from(grok_home_env: Option<OsString>, home_dir: Option<PathBuf>) -> PathBuf {
    grok_home_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir.unwrap_or_default().join(".grok"))
}

pub(crate) fn grok_catalog_context_window(home: &Path, model: &str) -> Option<u64> {
    let read = |name: &str| fs::read_to_string(home.join(name)).ok();
    read("models_cache.json")
        .and_then(|raw| grok_context_window_from_models_cache(&raw, model))
        .or_else(|| {
            read("config.toml").and_then(|raw| grok_context_window_from_config_toml(&raw, model))
        })
}

fn grok_context_window_from_models_cache(raw: &str, model: &str) -> Option<u64> {
    serde_json::from_str::<Value>(raw)
        .ok()?
        .get("models")?
        .get(model)?
        .get("info")?
        .get("context_window")?
        .as_u64()
        .filter(|window| *window > 0)
}

fn grok_context_window_from_config_toml(raw: &str, model: &str) -> Option<u64> {
    raw.parse::<toml::Table>()
        .ok()?
        .get("model")?
        .as_table()?
        .get(model)?
        .as_table()?
        .get("context_window")?
        .as_integer()
        .filter(|window| *window > 0)
        .map(|window| window as u64)
}

/// Read `[model.<id>].context_window` from `~/.grok/config.toml` (or `$GROK_HOME`).
///
/// Preference order:
/// 1. Exact `model_id` block when present
/// 2. `[models].default` block when `model_id` is absent or equals that default
///
/// Returns `None` when the file is missing, unreadable, or has no positive
/// `context_window` for the resolved model — callers fall back to model-family
/// inference.
pub fn read_grok_model_context_window(model_id: Option<&str>) -> Option<u64> {
    let raw = fs::read_to_string(resolve_grok_home_dir().join("config.toml")).ok()?;
    parse_grok_model_context_window_from_toml(&raw, model_id)
}

/// Pure TOML lookup for `[model.<id>].context_window` (testable without disk).
pub fn parse_grok_model_context_window_from_toml(
    raw_toml: &str,
    model_id: Option<&str>,
) -> Option<u64> {
    let table = raw_toml.parse::<toml::Table>().ok()?;
    let model_table = table.get("model")?.as_table()?;
    let lookup = |id: &str| -> Option<u64> {
        model_table
            .get(id)?
            .as_table()?
            .get("context_window")?
            .as_integer()
            .filter(|&n| n > 0)
            .map(|n| n as u64)
    };

    if let Some(id) = model_id.filter(|s| !s.is_empty()) {
        if let Some(cw) = lookup(id) {
            return Some(cw);
        }
        // Only fall through to default when the session model *is* the default
        // (or the exact block is missing but default points at a managed model
        // with the same id after a rename race). Different stock models keep
        // model-family inference.
        let default_id = table
            .get("models")
            .and_then(|m| m.as_table())
            .and_then(|m| m.get("default"))
            .and_then(|d| d.as_str())?;
        if default_id == id {
            return lookup(default_id);
        }
        return None;
    }

    let default_id = table
        .get("models")
        .and_then(|m| m.as_table())
        .and_then(|m| m.get("default"))
        .and_then(|d| d.as_str())?;
    lookup(default_id)
}

/// Resolve the context-window max for Grok: configured `context_window` wins
/// over model-family inference (settings UI writes `[model.<id>].context_window`).
pub fn resolve_grok_context_window_max_tokens(
    model_id: Option<&str>,
    configured: Option<u64>,
) -> u64 {
    if let Some(size) = configured.filter(|s| *s > 0) {
        return size;
    }
    infer_context_window_max_tokens(model_id.or(Some("grok"))).unwrap_or(256_000)
}

/// Grok Build (xAI) stores each conversation as a **directory-per-session**,
/// grouped by the (percent-encoded) working directory:
///
/// ```text
/// $GROK_HOME/                        (default ~/.grok)
/// └── sessions/
///     └── <percent-encoded-cwd>/     # e.g. %2FUsers%2Fme%2Fproj ; or slug+hash
///         │                          #   with a sibling `.cwd` file when >255 bytes
///         └── <session-uuid>/        # UUIDv7
///             ├── summary.json       # metadata index (see below)
///             ├── updates.jsonl      # ACP session/update stream — the conversation
///             ├── chat_history.jsonl # raw model messages (not read here)
///             ├── plan.json          # TODO state
///             └── terminal/<id>.log  # full background-command output
/// ```
///
/// `base_dir` points at the `sessions/` directory.
///
/// `summary.json` is the authoritative metadata source: `info.cwd`, timestamps,
/// `current_model_id`, `generated_title`/`session_summary`, `head_branch`, and
/// message counts. We read the working directory from here rather than decoding
/// the group directory name (which may be a slug+hash for long paths).
///
/// `updates.jsonl` is a newline-delimited **ACP `session/update` stream** — each
/// line is a JSON-RPC notification `{"method": "session/update" |
/// "_x.ai/session/update", "params": {"sessionId", "update": {…}}, "timestamp":
/// <unix secs>}`. The `update.sessionUpdate` discriminator is one of:
///
/// - `user_message_chunk` — a user prompt (`content.text`; `_meta.promptIndex`,
///   `_meta.modelId`). Starts a new user turn.
/// - `agent_message_chunk` — a complete assistant text segment (`content.text`).
///   NOT a streaming delta; a turn can contain several, interleaved with tools.
/// - `agent_thought_chunk` — a reasoning segment (`content.text`).
/// - `tool_call` — a tool invocation (`toolCallId`, `title`, `rawInput`,
///   `_meta["x.ai/tool"].{name,kind,label}`).
/// - `tool_call_update` — cumulative status/output for a call (`toolCallId`,
///   `status` ∈ {in_progress, completed, failed}, `content[]`, `rawOutput`).
///   The last update per `toolCallId` holds the full output.
/// - `task_backgrounded` / `task_completed` — a command that was moved to the
///   background. Both are ignored here (the launch call and the polls already
///   carry everything rendered); note `task_backgrounded` is the ONLY event
///   pairing `tool_call_id` with `task_id`, since `task_completed.task_snapshot`
///   is keyed by `task_id` alone.
/// - `turn_completed` — closes the current assistant turn (`stop_reason`).
///
/// Turn model: one user turn per `user_message_chunk`, then a single assistant
/// turn accumulating every reasoning/text/tool block until `turn_completed`
/// (or the next user prompt), preserving interleave order.
pub struct GrokParser {
    base_dir: PathBuf,
}

impl GrokParser {
    pub fn new() -> Self {
        Self {
            base_dir: resolve_grok_home_dir().join("sessions"),
        }
    }

    /// Construct a parser pointed at an explicit `sessions` directory (test
    /// fixtures).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn grok_home(&self) -> PathBuf {
        self.base_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.base_dir.clone())
    }

    fn build_summary(&self, session_dir: &Path, session_id: &str) -> Option<ConversationSummary> {
        let meta = read_summary_json(session_dir);
        if meta.session_kind.as_deref() == Some("subagent") {
            return None;
        }
        let parsed = parse_updates(&session_dir.join("updates.jsonl"), session_id);
        // A session that never produced any user/assistant/tool content (only
        // metadata) is treated as empty — matches the "metadata-only is not
        // listed" rule of the other parsers.
        if parsed.content_events == 0 {
            return None;
        }
        Some(self.summary_from(session_id, &meta, &parsed))
    }

    fn summary_from(
        &self,
        session_id: &str,
        meta: &SummaryMeta,
        parsed: &ParsedUpdates,
    ) -> ConversationSummary {
        let cwd = meta.cwd.clone();
        let folder_name = cwd.as_deref().map(folder_name_from_path);
        let title = visible_title(meta.title.clone())
            .or_else(|| parsed.first_user_text.as_deref().map(title_from_user_text));
        ConversationSummary {
            id: session_id.to_string(),
            agent_type: AgentType::Grok,
            folder_path: cwd,
            folder_name,
            title,
            started_at: meta.created_at.or(parsed.first_ts).unwrap_or_else(Utc::now),
            ended_at: meta.updated_at.or(parsed.last_ts),
            message_count: parsed.turns.len() as u32,
            model: meta.model.clone().or_else(|| parsed.model.clone()),
            git_branch: meta.git_branch.clone(),
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        }
    }

    fn build_detail(&self, session_dir: &Path, session_id: &str) -> ConversationDetail {
        let mut parsed = parse_updates(&session_dir.join("updates.jsonl"), session_id);
        let meta = read_summary_json(session_dir);

        // Defensive normalization shared with the other parsers: hoist any tool
        // result that landed outside its call's turn, and structurize file-read
        // output. Harmless no-ops when nothing matches.
        relocate_orphaned_tool_results(&mut parsed.turns);
        structurize_read_tool_output(&mut parsed.turns);

        // Grok resolves its native `ask_user_question` over the `_x.ai/ask_user_question`
        // ext round-trip and never writes the answer into `updates.jsonl`, so the
        // parsed ToolResult is empty and the `AskQuestionResultCard` shows "未选择".
        // Recover the user's picks from `chat_history.jsonl` (the model-facing
        // transcript, which DOES record the answer as a `tool_result`) and inject
        // them as the tool output. No-op when the file is absent or there's no ask.
        inject_grok_ask_answers(&mut parsed.turns, &session_dir.join("chat_history.jsonl"));

        // Fill assistant turns that carried no in-stream `modelId` with the
        // session model (summary `current_model_id`, else the first in-stream
        // model) so the message footer shows the model even for older/sparse
        // transcripts. Same for session-level `reasoning_effort` (Grok only
        // persists effort on summary.json, not per turn in updates.jsonl).
        if let Some(session_model) = meta.model.clone().or_else(|| parsed.model.clone()) {
            for turn in &mut parsed.turns {
                if matches!(turn.role, TurnRole::Assistant) && turn.model.is_none() {
                    turn.model = Some(session_model.clone());
                }
            }
        }
        if let Some(session_effort) = meta.reasoning_effort.clone() {
            for turn in &mut parsed.turns {
                if matches!(turn.role, TurnRole::Assistant) && turn.reasoning_effort.is_none() {
                    turn.reasoning_effort = Some(session_effort.clone());
                }
            }
        }

        // Grok times a turn from its own update spans; this reaches only the
        // turns whose updates carried no usable timestamps.
        backfill_turn_durations(&mut parsed.turns, &[]);

        // Grok sends no ACP `usage_update`, so the live meter stays empty; derive
        // the context ring here instead. The "used" figure is Grok's own
        // `params._meta.totalTokens` (`ParsedUpdates::context_tokens`) — NOT the
        // turn usage totals, which are the token SPEND and run to many multiples
        // of the resident context once a turn makes several model calls. Pair it
        // with the model's window so the status bar shows the ring (mirrors
        // gemini/kimi/opencode — the bare `compute_session_stats` leaves the
        // context fields `None`).
        let session_model = meta.model.as_deref().or(parsed.model.as_deref());
        let configured_max = session_model
            .and_then(|model| grok_catalog_context_window(&self.grok_home(), model))
            .or_else(|| read_grok_model_context_window(session_model));
        let max_tokens = Some(resolve_grok_context_window_max_tokens(
            session_model,
            configured_max,
        ));
        let session_stats = merge_context_window_stats(
            compute_session_stats(&parsed.turns),
            parsed
                .context_tokens
                .or_else(|| latest_turn_total_usage_tokens(&parsed.turns)),
            max_tokens,
        );
        let summary = self.summary_from(session_id, &meta, &parsed);

        ConversationDetail {
            summary,
            turns: parsed.turns,
            session_stats,
            transcript_watermark: Some(parsed.consumed_complete_bytes),
        }
    }

    /// Locate the `<session-uuid>` directory matching `conversation_id` across
    /// the `base_dir/<group>/` buckets (two shallow levels).
    pub(crate) fn find_session_dir(&self, conversation_id: &str) -> Option<PathBuf> {
        for group in read_subdirs(&self.base_dir) {
            let candidate = group.join(conversation_id);
            if candidate.join("updates.jsonl").is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn find_session_dir_loose(&self, conversation_id: &str) -> Option<PathBuf> {
        for group in read_subdirs(&self.base_dir) {
            let candidate = group.join(conversation_id);
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        None
    }
}

impl Default for GrokParser {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GrokSessionLocatorError {
    #[error("Grok sessions scan failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ambiguous Grok session id at {strictness:?} strictness ({count} matches)")]
    Ambiguous {
        strictness: GrokSessionMatchStrictness,
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokSessionMatchStrictness {
    Strict,
    Loose,
}

#[cfg(any(test, windows))]
fn is_unsupported_drive_relative_root_shape(has_root: bool, starts_with_prefix: bool) -> bool {
    !has_root && starts_with_prefix
}

#[cfg(windows)]
fn is_unsupported_drive_relative_sessions_root(path: &Path) -> bool {
    is_unsupported_drive_relative_root_shape(
        path.has_root(),
        matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        ),
    )
}

#[cfg(not(windows))]
fn is_unsupported_drive_relative_sessions_root(_path: &Path) -> bool {
    false
}

pub(crate) fn locate_grok_session_dir(
    sessions_root: &Path,
    session_id: &str,
) -> Result<Option<PathBuf>, GrokSessionLocatorError> {
    if is_unsupported_drive_relative_sessions_root(sessions_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Grok sessions root must not be drive-relative",
        )
        .into());
    }
    let entries = match fs::read_dir(sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match has_dangling_alias_component(sessions_root) {
                Ok(false) => return Ok(None),
                Ok(true) => return Err(error.into()),
                Err(other) => return Err(other.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let mut strict = Vec::new();
    let mut loose = Vec::new();

    for entry in entries {
        let entry = entry?;
        let group_type = entry.file_type()?;
        if !group_type.is_dir() && !group_type.is_symlink() {
            continue;
        }
        let candidate = entry.path().join(session_id);
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !candidate_metadata.is_dir() && !candidate_metadata.file_type().is_symlink() {
            continue;
        }
        loose.push(candidate.clone());

        match fs::metadata(candidate.join("updates.jsonl")) {
            Ok(metadata) if metadata.is_file() => strict.push(candidate),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    strict.sort();
    loose.sort();
    let (matches, strictness) = if strict.is_empty() {
        (&loose, GrokSessionMatchStrictness::Loose)
    } else {
        (&strict, GrokSessionMatchStrictness::Strict)
    };
    match matches.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => Err(GrokSessionLocatorError::Ambiguous {
            strictness,
            count: many.len(),
        }),
    }
}

impl AgentParser for GrokParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut conversations = Vec::new();
        if !self.base_dir.is_dir() {
            return Ok(conversations);
        }
        for group in read_subdirs(&self.base_dir) {
            for session_dir in read_subdirs(&group) {
                let Some(session_id) = session_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                else {
                    continue;
                };
                if let Some(summary) = self.build_summary(&session_dir, &session_id) {
                    conversations.push(summary);
                }
            }
        }
        conversations.sort_by_key(|c| std::cmp::Reverse(c.started_at));
        Ok(conversations)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        let session_dir = self
            .find_session_dir(conversation_id)
            .ok_or_else(|| ParseError::ConversationNotFound(conversation_id.to_string()))?;
        Ok(self.build_detail(&session_dir, conversation_id))
    }
}

/// Locate `updates.jsonl` for an external Grok session id the same way
/// [`GrokParser`] resolves conversation detail.
///
/// Returns the expected path when the session directory exists even if the
/// file has not been created yet, so a later tail can retry `is_file()`.
pub(crate) fn grok_updates_jsonl_path(session_id: &str) -> Option<PathBuf> {
    let parser = GrokParser::new();
    parser
        .find_session_dir(session_id)
        .or_else(|| parser.find_session_dir_loose(session_id))
        .map(|dir| dir.join("updates.jsonl"))
}

/// Assemble turns from complete `updates.jsonl` bytes with the same
/// normalization as cold parse. Returns `(turns, consumed_complete_bytes)`.
#[cfg(test)]
pub(crate) fn grok_turns_from_bytes(bytes: &[u8], session_id: &str) -> (Vec<MessageTurn>, u64) {
    let parsed = parse_updates_from_bytes(bytes, session_id);
    (parsed.turns, parsed.consumed_complete_bytes)
}

/// Normalize one bounded autonomous episode segment. The hidden trigger may
/// live in a previous scanner batch, so its proven context is supplied by the
/// observer while record offsets remain absolute for stable identity.
pub(crate) fn grok_autonomous_turn_from_segment(
    bytes: &[u8],
    session_id: &str,
    base_offset: u64,
    trigger_start: u64,
    task_ids: &[String],
    candidate_task_ids: &[String],
    allow_legacy_terminal: bool,
) -> Option<MessageTurn> {
    parse_updates_from_bytes_with_context(
        bytes,
        session_id,
        base_offset,
        Some(PendingGrokAutonomous {
            trigger_start,
            task_ids: task_ids.to_vec(),
            candidate_task_ids: candidate_task_ids.to_vec(),
            wake_generations: Vec::new(),
            allow_legacy_terminal,
        }),
        true,
    )
    .turns
    .into_iter()
    .find(|turn| turn.autonomous_origin == Some(AutonomousTurnOrigin::BackgroundTask))
}

// ---------------------------------------------------------------------------
// summary.json
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SummaryMeta {
    cwd: Option<String>,
    title: Option<String>,
    model: Option<String>,
    /// Session-level reasoning effort from `summary.json` (`reasoning_effort`).
    /// Grok does not write per-turn effort into `updates.jsonl`; this is the
    /// archive value stamped onto assistant turns on history reload.
    reasoning_effort: Option<String>,
    git_branch: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    session_kind: Option<String>,
}

/// Cheap archive peek for workflow cards: `(model, reasoning_effort)` from
/// `summary.json` only (no updates.jsonl parse). `session_id` is the Grok
/// session uuid stored as the child conversation's `external_id`.
pub fn peek_session_model_and_effort(session_id: &str) -> (Option<String>, Option<String>) {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return (None, None);
    }
    let parser = GrokParser::new();
    let Some(session_dir) = parser.find_session_dir(session_id) else {
        return (None, None);
    };
    let meta = read_summary_json(&session_dir);
    (meta.model, meta.reasoning_effort)
}

fn read_summary_json(session_dir: &Path) -> SummaryMeta {
    let Ok(raw) = fs::read_to_string(session_dir.join("summary.json")) else {
        return SummaryMeta::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return SummaryMeta::default();
    };
    let non_empty = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    SummaryMeta {
        cwd: v
            .pointer("/info/cwd")
            .and_then(Value::as_str)
            .and_then(non_empty),
        // `generated_title` is the model-generated title; `session_summary` is
        // the fallback one-liner. Prefer the title.
        title: v
            .get("generated_title")
            .and_then(Value::as_str)
            .and_then(non_empty)
            .or_else(|| {
                v.get("session_summary")
                    .and_then(Value::as_str)
                    .and_then(non_empty)
            }),
        model: v
            .get("current_model_id")
            .and_then(Value::as_str)
            .and_then(non_empty),
        reasoning_effort: v
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .and_then(non_empty),
        git_branch: v
            .get("head_branch")
            .and_then(Value::as_str)
            .and_then(non_empty),
        created_at: v
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339),
        updated_at: v
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339),
        session_kind: v
            .get("session_kind")
            .and_then(Value::as_str)
            .and_then(non_empty),
    }
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// updates.jsonl
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ParsedUpdates {
    turns: Vec<MessageTurn>,
    first_ts: Option<DateTime<Utc>>,
    last_ts: Option<DateTime<Utc>>,
    content_events: u32,
    first_user_text: Option<String>,
    /// Model discovered in-stream (`user_message_chunk._meta.modelId`); a
    /// fallback when `summary.json` lacks `current_model_id`.
    model: Option<String>,
    /// Bytes of complete `updates.jsonl` lines consumed, including each
    /// trailing `\n`. A trailing partial line is not counted.
    consumed_complete_bytes: u64,
    /// Context-window occupancy: `params._meta.totalTokens` as of the last turn
    /// to state one. Feeds the context ring, and is kept apart from turn `usage`
    /// because the two measure different things — occupancy is what currently
    /// sits in the window, `usage` is what the turn spent getting there.
    context_tokens: Option<u64>,
}

/// Idle-boundary hidden trigger waiting to stamp the next independently
/// opened assistant turn.
struct PendingGrokAutonomous {
    trigger_start: u64,
    task_ids: Vec<String>,
    candidate_task_ids: Vec<String>,
    wake_generations: Vec<(String, u64)>,
    allow_legacy_terminal: bool,
}

/// Canonical id for a Grok idle-boundary autonomous assistant turn.
///
/// Public format: `grok-autonomous:<episode-key>:assistant:0`
///
/// Episode-key UTF-8 material:
/// - with task ids: `{session_id}+{sorted_task_ids}+{trigger_start_offset}`
///   (`sorted_task_ids` is the referenced set, lexicographically sorted and
///   joined by `,`)
/// - without task ids: `{session_id}+{trigger_start_offset}`
///
/// `session_id` is the external Grok session uuid. `trigger_start_offset` is
/// the hidden trigger's complete-line **start** byte offset in
/// `updates.jsonl` (decimal, no leading zeros except `0`).
///
/// If that raw key contains a character outside `[A-Za-z0-9._+,-]`, it is
/// replaced by the lowercase hex SHA-256 of the same UTF-8 material so the
/// public id stays a legal token. Task 5 must call this function rather than
/// re-deriving the key.
pub(crate) fn grok_autonomous_turn_id(
    session_id: &str,
    task_ids: &[String],
    trigger_start_offset: u64,
) -> String {
    let key = grok_autonomous_episode_key(session_id, task_ids, trigger_start_offset);
    format!("grok-autonomous:{key}:assistant:0")
}

fn grok_autonomous_episode_key(
    session_id: &str,
    task_ids: &[String],
    trigger_start_offset: u64,
) -> String {
    let mut ids: Vec<&str> = task_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();
    let raw = if ids.is_empty() {
        format!("{session_id}+{trigger_start_offset}")
    } else {
        format!("{session_id}+{}+{trigger_start_offset}", ids.join(","))
    };
    if grok_episode_key_is_legal(&raw) {
        raw
    } else {
        grok_episode_key_digest(raw.as_bytes())
    }
}

fn grok_episode_key_is_legal(key: &str) -> bool {
    !key.is_empty()
        && key.bytes().all(|b| {
            matches!(
                b,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' | b'+' | b','
            )
        })
}

fn grok_episode_key_digest(material: &[u8]) -> String {
    let digest = Sha256::digest(material);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Iterate complete newline-terminated records in `bytes`.
///
/// Each item is `(start_offset, record)` where `record` includes the trailing
/// `\n`. A trailing fragment without `\n` is omitted and is not part of the
/// consumed-byte count (`start_offset + record.len()` of the last item, or 0).
pub(crate) fn grok_complete_records(bytes: &[u8]) -> impl Iterator<Item = (u64, &[u8])> {
    let mut start = 0usize;
    std::iter::from_fn(move || {
        let rest = bytes.get(start..)?;
        let rel = rest.iter().position(|&b| b == b'\n')?;
        let end = start + rel + 1;
        let rec = &bytes[start..end];
        let off = start as u64;
        start = end;
        Some((off, rec))
    })
}

pub(crate) fn grok_record_payload(record: &[u8]) -> &[u8] {
    let without_nl = record.strip_suffix(b"\n").unwrap_or(record);
    without_nl.strip_suffix(b"\r").unwrap_or(without_nl)
}

pub(crate) fn is_grok_background_task_reminder(text: &str) -> bool {
    !grok_reminder_task_ids(text).is_empty()
}

/// Task IDs from the verified Grok Bash and Monitor completion templates.
/// Does not scan arbitrary English continuation text.
pub(crate) fn grok_reminder_task_ids(text: &str) -> Vec<String> {
    let Some(body) = text
        .trim()
        .strip_prefix("<system-reminder>")
        .and_then(|text| text.strip_suffix("</system-reminder>"))
    else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for line in body.lines().map(str::trim) {
        let shape = if line.starts_with("Background task \"") {
            Some(("Background task \"", " completed (", ")."))
        } else if line.starts_with("Monitor \"") {
            Some(("Monitor \"", " ended: [monitor ended: ", "]."))
        } else {
            None
        };
        let Some((prefix, middle, suffix)) = shape else {
            continue;
        };
        let after_prefix = &line[prefix.len()..];
        let Some(quote) = after_prefix.find('"') else {
            continue;
        };
        let id = &after_prefix[..quote];
        let rest = &after_prefix[quote + 1..];
        if !id.is_empty() && rest.starts_with(middle) && rest.ends_with(suffix) {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn task_completion_wake_disposition(update: &Value) -> Option<(String, Option<bool>)> {
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("task_completed") {
        return None;
    }
    let task_id = update
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| {
            update
                .pointer("/task_snapshot/task_id")
                .and_then(Value::as_str)
        })
        .filter(|id| !id.is_empty())
        .map(str::to_string)?;
    Some((task_id, update.get("will_wake").and_then(Value::as_bool)))
}

pub(crate) fn grok_task_completed_prompt_task_id(update: &Value) -> Option<&str> {
    update
        .get("prompt_id")
        .and_then(Value::as_str)
        .and_then(|prompt_id| prompt_id.strip_prefix("task-completed-"))
        .filter(|task_id| !task_id.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokAutonomousTerminalMatch<'a> {
    Legacy,
    Task(&'a str),
}

pub(crate) fn grok_autonomous_terminal_match<'a>(
    update: &'a Value,
    task_ids: &[String],
    candidate_task_ids: &[String],
    allow_legacy_terminal: bool,
) -> Option<GrokAutonomousTerminalMatch<'a>> {
    let Some(prompt_id) = update.get("prompt_id") else {
        return (allow_legacy_terminal && candidate_task_ids.is_empty() && !task_ids.is_empty())
            .then_some(GrokAutonomousTerminalMatch::Legacy);
    };
    let prompt_id = prompt_id.as_str()?;
    let task_id = prompt_id
        .strip_prefix("task-completed-")
        .filter(|task_id| !task_id.is_empty())?;
    let expected_ids = if candidate_task_ids.is_empty() {
        task_ids
    } else {
        candidate_task_ids
    };
    expected_ids
        .iter()
        .any(|expected| expected == task_id)
        .then_some(GrokAutonomousTerminalMatch::Task(task_id))
}

fn parse_updates(path: &Path, session_id: &str) -> ParsedUpdates {
    let Ok(bytes) = fs::read(path) else {
        return ParsedUpdates::default();
    };
    parse_updates_from_bytes(&bytes, session_id)
}

fn parse_updates_from_bytes(bytes: &[u8], session_id: &str) -> ParsedUpdates {
    parse_updates_from_bytes_with_context(bytes, session_id, 0, None, false)
}

fn parse_updates_from_bytes_with_context(
    bytes: &[u8],
    session_id: &str,
    base_offset: u64,
    initial_pending: Option<PendingGrokAutonomous>,
    retain_unresolved_at_eof: bool,
) -> ParsedUpdates {
    let mut out = ParsedUpdates::default();
    // The in-flight assistant turn, plus a `toolCallId → index-of-its-ToolResult`
    // map scoped to that turn (cleared on every turn boundary).
    let mut assistant: Option<MessageTurn> = None;
    let mut tool_result_idx: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // Stats for the in-flight turn (tokens/timing/model), applied to the
    // assistant turn when it is finalized. Reset at each turn boundary.
    let mut turn_meta = GrokTurnMeta::default();
    // `promptIndex` of the currently-open user turn. Grok splits one prompt into
    // several `user_message_chunk`s (prose, image, …) sharing a `promptIndex`;
    // this lets consecutive same-prompt chunks merge into a single user turn
    // instead of each opening a new (often empty) one.
    let mut open_user_prompt_index: Option<i64> = None;
    let mut pending_autonomous = initial_pending;
    let mut active_autonomous: Option<PendingGrokAutonomous> = None;
    let mut expected_wakes = std::collections::VecDeque::<(String, u64)>::new();
    let mut structured_wake_dispositions = std::collections::HashSet::<String>::new();
    let mut recent_completion_ids = std::collections::HashSet::<String>::new();
    let mut foreground_invalidated_ids = std::collections::HashSet::<String>::new();
    let mut consumed_complete_bytes = 0u64;

    for (relative_start, record) in grok_complete_records(bytes) {
        let start_offset = base_offset.saturating_add(relative_start);
        consumed_complete_bytes = start_offset + record.len() as u64;
        let payload = grok_record_payload(record);
        let Ok(line) = std::str::from_utf8(payload) else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        let ts = v
            .get("timestamp")
            .and_then(Value::as_i64)
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single());
        if let Some(t) = ts {
            if out.first_ts.is_none() {
                out.first_ts = Some(t);
            }
            out.last_ts = Some(t);
        }
        let now = ts.unwrap_or_else(Utc::now);

        let Some(update) = v.pointer("/params/update") else {
            continue;
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Some((task_id, will_wake)) = task_completion_wake_disposition(update) {
            if pending_autonomous.as_ref().is_some_and(|pending| {
                pending.task_ids.iter().any(|id| id == &task_id)
                    || pending.candidate_task_ids.iter().any(|id| id == &task_id)
            }) {
                pending_autonomous = None;
            }
            expected_wakes.retain(|(id, _)| id != &task_id);
            structured_wake_dispositions.remove(&task_id);
            foreground_invalidated_ids.remove(&task_id);
            recent_completion_ids.insert(task_id.clone());
            if let Some(will_wake) = will_wake {
                structured_wake_dispositions.insert(task_id.clone());
                if will_wake {
                    expected_wakes.push_back((task_id, start_offset));
                }
            }
        }

        // Grok injects its own reminders (a background task finishing, …) as
        // `user_message_chunk`s and marks them `_meta.hideFromScrollback` — its
        // TUI never shows them. Honor the flag: rendered as a user bubble, such a
        // chunk splits one reply into two turns with a raw `<system-reminder>`
        // block wedged between them. Skipped BEFORE the turn-boundary logic below,
        // so a reminder injected mid-turn doesn't cut the open assistant turn
        // either.
        if kind == "user_message_chunk"
            && update
                .pointer("/_meta/hideFromScrollback")
                .and_then(Value::as_bool)
                == Some(true)
        {
            // Hidden reminders never become user turns. At an idle boundary
            // (assistant already flushed) the verified background-task shape
            // stamps the *next* independently opened assistant; mid-assistant
            // injection is suppress-only and does not relabel the open turn.
            if assistant.is_none() && pending_autonomous.is_none() {
                // Parked T4: a hidden trigger after a committed User turn and
                // before that prompt's assistant is not an idle-boundary
                // follow-up. The verified sequence is trigger then assistant
                // after an idle boundary (last committed turn is not User).
                let last_is_user = matches!(
                    out.turns.last(),
                    Some(t) if matches!(t.role, TurnRole::User)
                );
                if !last_is_user {
                    let text = update_text(update);
                    let legacy_task_ids = grok_reminder_task_ids(&text);
                    let matching_structured: Vec<String> = legacy_task_ids
                        .iter()
                        .filter(|id| expected_wakes.iter().any(|(pending, _)| pending == *id))
                        .cloned()
                        .collect();
                    let legacy_fallback_task_ids: Vec<String> = legacy_task_ids
                        .iter()
                        .filter(|id| {
                            !structured_wake_dispositions.contains(*id)
                                && !foreground_invalidated_ids.contains(*id)
                        })
                        .cloned()
                        .collect();
                    let (task_ids, candidate_task_ids, allow_legacy_terminal) =
                        if !matching_structured.is_empty() {
                            (matching_structured, Vec::new(), false)
                        } else if !legacy_fallback_task_ids.is_empty() {
                            (legacy_fallback_task_ids, Vec::new(), true)
                        } else if expected_wakes.len() == 1 {
                            (
                                expected_wakes
                                    .front()
                                    .map(|(id, _)| vec![id.clone()])
                                    .unwrap_or_default(),
                                Vec::new(),
                                false,
                            )
                        } else if !expected_wakes.is_empty() {
                            (
                                Vec::new(),
                                expected_wakes.iter().map(|(id, _)| id.clone()).collect(),
                                false,
                            )
                        } else {
                            (Vec::new(), Vec::new(), false)
                        };
                    if !task_ids.is_empty() || !candidate_task_ids.is_empty() {
                        let selected_ids = if candidate_task_ids.is_empty() {
                            task_ids.as_slice()
                        } else {
                            candidate_task_ids.as_slice()
                        };
                        let wake_generations: Vec<(String, u64)> = expected_wakes
                            .iter()
                            .filter(|(id, _)| selected_ids.contains(id))
                            .cloned()
                            .collect();
                        if candidate_task_ids.is_empty() {
                            expected_wakes.retain(|wake| !wake_generations.contains(wake));
                        }
                        pending_autonomous = Some(PendingGrokAutonomous {
                            trigger_start: start_offset,
                            task_ids,
                            candidate_task_ids,
                            wake_generations,
                            allow_legacy_terminal,
                        });
                    }
                } else {
                    expected_wakes.clear();
                }
            } else if assistant.is_some() {
                expected_wakes.clear();
            }
            continue;
        }

        // Grok's per-turn stats live in the OUTER `params._meta` (token total +
        // timing) plus `update._meta.modelId`. Accumulate them into `turn_meta`
        // and apply at the turn boundary. A `user_message_chunk` that opens a NEW
        // prompt closes+resets the prior turn's accumulator; a continuation chunk
        // of the SAME prompt (see below) keeps accumulating.
        let params_meta = v.pointer("/params/_meta");
        let update_meta = update.get("_meta");
        // Grok emits each content piece of one prompt (prose, image, …) as its
        // own `user_message_chunk` sharing a `promptIndex`. Merge consecutive
        // user chunks of the same prompt into ONE user turn so a "text + image"
        // prompt renders as a single bubble (matching the live path) rather than
        // a trailing empty/image-only turn. A chunk continues the open user turn
        // when no assistant content has intervened and the `promptIndex` matches
        // (or is absent on either side).
        let user_chunk_continues = kind == "user_message_chunk"
            && assistant.is_none()
            && matches!(out.turns.last(), Some(t) if matches!(t.role, TurnRole::User))
            && update
                .pointer("/_meta/promptIndex")
                .and_then(Value::as_i64)
                .zip(open_user_prompt_index)
                .is_none_or(|(a, b)| a == b);
        if kind == "user_message_chunk" && !user_chunk_continues {
            if let Some(prev) = assistant.as_mut() {
                turn_meta.apply(prev, &mut out.context_tokens);
            }
            if active_autonomous
                .as_ref()
                .is_some_and(|active| !active.candidate_task_ids.is_empty())
            {
                assistant.take();
                tool_result_idx.clear();
            } else {
                flush_assistant(&mut assistant, &mut out.turns, &mut tool_result_idx);
            }
            active_autonomous = None;
            turn_meta = GrokTurnMeta::default();
            // A visible user prompt is not an autonomous follow-up.
            pending_autonomous = None;
            expected_wakes.clear();
            foreground_invalidated_ids.extend(recent_completion_ids.drain());
        }
        turn_meta.observe(params_meta, update_meta);

        match kind {
            "user_message_chunk" => {
                let block = user_chunk_to_block(update).and_then(|block| match block {
                    ContentBlock::Text { text } => {
                        visible_user_text(&text).map(|text| ContentBlock::Text { text })
                    }
                    other => Some(other),
                });
                let Some(block) = block else {
                    continue;
                };
                out.content_events += 1;
                // Title/first-prompt text comes only from prose chunks; an image
                // chunk carries no text and must not overwrite it.
                if let ContentBlock::Text { text } = &block {
                    if out.first_user_text.is_none() && !text.trim().is_empty() {
                        out.first_user_text = Some(text.clone());
                    }
                }
                if out.model.is_none() {
                    out.model = update
                        .pointer("/_meta/modelId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                if user_chunk_continues {
                    // Same prompt: append the block to the open user turn.
                    if let Some(turn) = out.turns.last_mut() {
                        turn.blocks.push(block);
                    }
                } else {
                    open_user_prompt_index =
                        update.pointer("/_meta/promptIndex").and_then(Value::as_i64);
                    out.turns.push(MessageTurn {
                        id: String::new(), // assigned in a final pass
                        role: TurnRole::User,
                        blocks: vec![block],
                        timestamp: now,
                        usage: None,
                        duration_ms: None,
                        model: None,
                        reasoning_effort: None,
                        completed_at: None,
                        outcome: None,
                        autonomous_origin: None,
                        generation_ms: None,
                        generation_tokens: None,
                    });
                }
            }
            "agent_message_chunk" => {
                out.content_events += 1;
                let text = update_text(update);
                append_text(
                    ensure_assistant(
                        &mut assistant,
                        now,
                        session_id,
                        &mut pending_autonomous,
                        &mut active_autonomous,
                    ),
                    text,
                );
            }
            "agent_thought_chunk" => {
                out.content_events += 1;
                let text = update_text(update);
                append_thinking(
                    ensure_assistant(
                        &mut assistant,
                        now,
                        session_id,
                        &mut pending_autonomous,
                        &mut active_autonomous,
                    ),
                    text,
                );
            }
            "tool_call" => {
                out.content_events += 1;
                let id = str_field(update, "toolCallId");
                // Grok wraps every MCP call in a `use_tool` envelope; peel it so
                // the call is classified/parsed as a direct MCP call (matches the
                // live path, connection.rs::unwrap_grok_use_tool). Native tools —
                // whose args are top-level — pass through unchanged.
                let raw_input = update.get("rawInput");
                let unwrapped = unwrap_use_tool(raw_input);
                let tool_name = match unwrapped {
                    Some((name, _)) => name.to_string(),
                    None => update
                        .get("_meta")
                        .and_then(|m| m.get("x.ai/tool"))
                        .and_then(|t| t.get("name"))
                        .and_then(Value::as_str)
                        .or_else(|| update.get("title").and_then(Value::as_str))
                        .unwrap_or("tool")
                        .to_string(),
                };
                let input_preview = match unwrapped {
                    // Valid-JSON-preserving cap so the delegation card can parse a
                    // long task; native inputs keep the opaque byte-truncation.
                    Some((_, input)) => grok_mcp_input_preview(input),
                    None => tool_input_preview(raw_input),
                };
                let turn = ensure_assistant(
                    &mut assistant,
                    now,
                    session_id,
                    &mut pending_autonomous,
                    &mut active_autonomous,
                );
                turn.blocks.push(ContentBlock::ToolUse {
                    tool_use_id: Some(id.clone()),
                    tool_name,
                    input_preview,
                    meta: None,

                    status: None,
                });
                turn.blocks.push(ContentBlock::ToolResult {
                    tool_use_id: Some(id.clone()),
                    output_preview: None,
                    is_error: false,
                    agent_stats: None,
                    images: Vec::new(),
                });
                if !id.is_empty() {
                    tool_result_idx.insert(id, turn.blocks.len() - 1);
                }
            }
            "tool_call_update" => {
                let id = str_field(update, "toolCallId");
                let output = update_tool_output(update);
                let failed = update.get("status").and_then(Value::as_str) == Some("failed");
                apply_tool_result(assistant.as_mut(), &tool_result_idx, &id, output, failed);
            }
            "turn_completed" => {
                let autonomous_terminal = active_autonomous
                    .as_ref()
                    .or(pending_autonomous.as_ref())
                    .map(|active| {
                        grok_autonomous_terminal_match(
                            update,
                            &active.task_ids,
                            &active.candidate_task_ids,
                            active.allow_legacy_terminal,
                        )
                    });
                if autonomous_terminal.is_some_and(|matched| matched.is_none()) {
                    continue;
                }
                if assistant.is_none() {
                    if let Some(pending) = pending_autonomous.take() {
                        if !pending.candidate_task_ids.is_empty() {
                            if let Some(GrokAutonomousTerminalMatch::Task(task_id)) =
                                autonomous_terminal.flatten()
                            {
                                if let Some(generation) =
                                    pending
                                        .wake_generations
                                        .iter()
                                        .find_map(|(id, generation)| {
                                            (id == task_id).then_some(*generation)
                                        })
                                {
                                    expected_wakes.retain(|(id, pending_generation)| {
                                        id != task_id || *pending_generation != generation
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(mut turn) = assistant.take() {
                    if let Some(active) = active_autonomous.take() {
                        if !active.candidate_task_ids.is_empty() {
                            if let Some(GrokAutonomousTerminalMatch::Task(task_id)) =
                                autonomous_terminal.flatten()
                            {
                                let resolved = vec![task_id.to_string()];
                                turn.id = grok_autonomous_turn_id(
                                    session_id,
                                    &resolved,
                                    active.trigger_start,
                                );
                                if let Some(generation) =
                                    active.wake_generations.iter().find_map(|(id, generation)| {
                                        (id == task_id).then_some(*generation)
                                    })
                                {
                                    expected_wakes.retain(|(id, pending_generation)| {
                                        id != task_id || *pending_generation != generation
                                    });
                                }
                            }
                        }
                    }
                    // The one place Grok states real token spend, scoped to the
                    // prompt this update closes (see `prompt_usage`).
                    turn_meta.usage = update.get("usage").and_then(prompt_usage);
                    turn_meta.apply(&mut turn, &mut out.context_tokens);
                    turn.completed_at = Some(now);
                    out.turns.push(turn);
                }
                active_autonomous = None;
                turn_meta = GrokTurnMeta::default();
                tool_result_idx.clear();
            }
            // Grok's auto-compaction (`/compact` or threshold-triggered) lands in
            // updates.jsonl on the namespaced `_x.ai/session/update` method as
            // `auto_compact_completed` {tokens_before, tokens_after}. Mirror the
            // live synthesis (connection.rs::map_grok_ext_notification): a completed
            // ToolUse tagged `meta.contextCompaction` so the history path renders the
            // shared ContextCompactionCard with the token delta instead of dropping
            // it. The paired ToolResult keeps the block well-formed (no orphan
            // tool_use). The `/compact` command itself is not recoverable — Grok
            // never persists slash commands as `user_message_chunk`s — so only the
            // compaction OUTCOME shows, not a "/compact" user bubble.
            "auto_compact_completed" => {
                out.content_events += 1;
                let mut meta = serde_json::Map::new();
                meta.insert("contextCompaction".to_string(), Value::Bool(true));
                if let Some(before) = update.get("tokens_before").and_then(Value::as_u64) {
                    meta.insert("tokensBefore".to_string(), before.into());
                }
                if let Some(after) = update.get("tokens_after").and_then(Value::as_u64) {
                    meta.insert("tokensAfter".to_string(), after.into());
                }
                // A stable id from the event id (deterministic across re-parses);
                // the paired blocks are self-contained, so no `tool_result_idx`
                // registration or later update is needed.
                let id = params_meta
                    .and_then(|m| m.get("eventId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("grok-compaction-{}", out.content_events));
                let turn = ensure_assistant(
                    &mut assistant,
                    now,
                    session_id,
                    &mut pending_autonomous,
                    &mut active_autonomous,
                );
                turn.blocks.push(ContentBlock::ToolUse {
                    tool_use_id: Some(id.clone()),
                    tool_name: "context_compaction".to_string(),
                    input_preview: None,
                    meta: Some(Value::Object(meta)),

                    status: None,
                });
                turn.blocks.push(ContentBlock::ToolResult {
                    tool_use_id: Some(id),
                    output_preview: None,
                    is_error: false,
                    agent_stats: None,
                    images: Vec::new(),
                });
            }
            // `task_backgrounded` / `task_completed` / plan / other extension
            // updates carry no distinct rendered content beyond what the tool
            // stream already has.
            //
            // In particular `task_completed`'s snapshot is deliberately NOT
            // applied to the launching tool call: Grok reports that CALL as
            // `completed` on the wire (it did start the task). The live
            // autonomous adapter consumes this extension for lifecycle evidence
            // but likewise does not render its snapshot; the task's real
            // outcome — command, exit code, output — renders from the
            // `get_command_or_subagent_output` polls (see
            // `grok_task_output_envelope`). Writing the snapshot here would make
            // history contradict live for the same conversation.
            //
            // Known gap: a task the model never polls has its output ONLY in
            // this snapshot, so neither path surfaces it.
            _ => {}
        }
    }

    // A session that ends mid-turn (no trailing `turn_completed`) still gets its
    // accumulated stats. Its `usage` stays `None` — Grok has not stated the
    // spend yet — but the context occupancy it did state still reaches the ring.
    if let Some(prev) = assistant.as_mut() {
        turn_meta.apply(prev, &mut out.context_tokens);
    }
    if !retain_unresolved_at_eof
        && active_autonomous
            .as_ref()
            .is_some_and(|active| !active.candidate_task_ids.is_empty())
    {
        assistant.take();
        tool_result_idx.clear();
    } else {
        flush_assistant(&mut assistant, &mut out.turns, &mut tool_result_idx);
    }
    out.consumed_complete_bytes = consumed_complete_bytes;

    // Assign stable, unique, index-based ids (the transcript is append-only, so
    // positional ids are stable across re-parses). Recognized autonomous
    // assistants already have their canonical id — do not overwrite them.
    for (i, turn) in out.turns.iter_mut().enumerate() {
        if turn.id.is_empty() {
            turn.id = format!("grok-turn-{i}");
        }
    }
    out
}

fn update_text(update: &Value) -> String {
    update
        .pointer("/content/text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Classify a `user_message_chunk`'s `content` into a display block.
///
/// Grok sends prose as `{type:"text"}`. Current codeg prompts send a native
/// `{type:"image"}` chunk (so grok's describe sidecar runs). Older transcripts
/// still carry the embedded `{type:"resource", resource:{blob, mimeType, uri}}`
/// shape from when we followed grok's `image:false` advertisement. Both
/// image-mime forms become [`ContentBlock::Image`] so they render as a
/// thumbnail; a non-image embedded resource folds to a `[uri](uri)` link
/// (same as the live [`crate::acp::user_blocks_from_prompt`]). Anything else
/// falls back to a (possibly empty) text block.
fn user_chunk_to_block(update: &Value) -> Option<ContentBlock> {
    let content = update.get("content")?;
    match content.get("type").and_then(Value::as_str).unwrap_or("") {
        "resource" => {
            let resource = content.get("resource")?;
            let mime = resource.get("mimeType").and_then(Value::as_str);
            let blob = resource.get("blob").and_then(Value::as_str);
            match (mime, blob) {
                (Some(mime), Some(blob)) if mime.starts_with("image/") => {
                    Some(ContentBlock::Image {
                        data: blob.to_string(),
                        mime_type: mime.to_string(),
                        uri: resource
                            .get("uri")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                }
                _ => {
                    let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
                    Some(ContentBlock::Text {
                        text: format!("[{uri}]({uri})"),
                    })
                }
            }
        }
        // Native ACP image content — the live send path for every grok that
        // decodes the format (see `normalize_grok_image_blocks`).
        "image" => {
            let data = content.get("data").and_then(Value::as_str)?;
            Some(ContentBlock::Image {
                data: data.to_string(),
                mime_type: content
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string(),
                uri: content
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
        // "text" and unknown kinds: existing behavior (reads `/content/text`).
        _ => Some(ContentBlock::Text {
            text: update_text(update),
        }),
    }
}

// ---------------------------------------------------------------------------
// chat_history.jsonl — grok native ask_user_question answers
// ---------------------------------------------------------------------------

/// Inject the user's `ask_user_question` picks — recorded only in
/// `chat_history.jsonl`, never in `updates.jsonl` — into the matching ToolResult
/// so the `AskQuestionResultCard` renders the answer instead of "未选择". Mirrors
/// the live path (`connection.rs::handle_grok_ask_user_question`): both feed the
/// card the same `{answers, declined}` envelope with an empty `header`, so a
/// conversation renders identically live and after reload. No-op when there is no
/// ask or `chat_history.jsonl` is absent.
fn inject_grok_ask_answers(turns: &mut [MessageTurn], chat_history: &Path) {
    // The native ask carries meta `x.ai/tool.kind == "ask_user"`, which the
    // tool_call arm mapped to this tool name; collect those call ids.
    let ask_ids: std::collections::HashSet<String> = turns
        .iter()
        .flat_map(|t| t.blocks.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse {
                tool_use_id: Some(id),
                tool_name,
                ..
            } if tool_name == GROK_ASK_TOOL_NAME => Some(id.clone()),
            _ => None,
        })
        .collect();
    if ask_ids.is_empty() {
        return;
    }
    let answers = read_grok_ask_answers(chat_history, &ask_ids);
    if answers.is_empty() {
        return;
    }
    for turn in turns.iter_mut() {
        for block in turn.blocks.iter_mut() {
            if let ContentBlock::ToolResult {
                tool_use_id: Some(id),
                output_preview,
                is_error,
                ..
            } = block
            {
                if let Some(env) = answers.get(id) {
                    *output_preview = Some(env.clone());
                    *is_error = false;
                }
            }
        }
    }
}

/// Read `chat_history.jsonl` and, for each `tool_result` whose `tool_call_id` is a
/// known ask id, parse its content into the `{answers, declined}` envelope JSON.
/// `chat_history.jsonl` is grok's model-facing transcript; an ask result there is
/// `{type:"tool_result", tool_call_id, content}` and its id matches the
/// `updates.jsonl` call id verbatim. Empty map when the file is missing.
fn read_grok_ask_answers(
    chat_history: &Path,
    ask_ids: &std::collections::HashSet<String>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(file) = fs::File::open(chat_history) else {
        return out;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = v.get("tool_call_id").and_then(Value::as_str) else {
            continue;
        };
        if !ask_ids.contains(id) {
            continue;
        }
        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
        if let Some(envelope) = grok_history_answer_to_envelope(content) {
            out.insert(id.to_string(), envelope.to_string());
        }
    }
    out
}

/// Parse a grok `ask_user_question` `tool_result` content string into the codeg
/// `{answers, declined}` envelope (the shape `parseAskQuestionOutcome` reads).
///
/// Verified against grok-0.2.101. The accepted template is `User has answered
/// your questions: "Q"="A", "Q2"="B, C". You can now …` (a multi-select value is
/// joined with `, `); the declined / skip_interview template is `The user has
/// indicated they have provided enough answers …` / `(No answer provided)`.
///
/// `header` is emitted empty to match the header-less card input (grok's questions
/// carry no header). Returns `None` for anything that is not one of these shapes,
/// leaving the ToolResult untouched (today's behavior) — safe by construction.
fn grok_history_answer_to_envelope(content: &str) -> Option<Value> {
    let content = content.trim();
    // Declined / skip_interview: distinct template, no per-question picks to show.
    if content.starts_with("The user has indicated they have provided enough answers")
        || content.contains("(No answer provided)")
    {
        return Some(serde_json::json!({ "answers": [], "declined": true }));
    }
    // Accepted: only this exact prefix (English — grok's internal template, not
    // localized) carries `"Q"="A"` pairs.
    if !content.starts_with("User has answered your questions:") {
        return None;
    }
    // Split on the `"` delimiter. For `"Q1"="A1", "Q2"="A2". You can now …` the
    // tokens are ["…: ", Q1, "=", A1, ", ", Q2, "=", A2, ". You can now …"], so a
    // pair is (toks[i], toks[i+2]) with toks[i+1] == "=", advancing by 4. Trailing
    // prose after the last quote is ignored. Lossy only if a question or label
    // contains a literal `"` (then that pair's `=` guard fails and we stop) —
    // questions rarely do, matching the existing text-fallback's tolerance.
    let toks: Vec<&str> = content.split('"').collect();
    let mut answers: Vec<Value> = Vec::new();
    let mut i = 1;
    while i + 2 < toks.len() {
        if toks[i + 1] != "=" {
            break;
        }
        let question = toks[i];
        // Multi-select values are joined with ", "; split them back into the label
        // array the card partitions against the offered options.
        let selected: Vec<String> = toks[i + 2]
            .split(", ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        answers.push(serde_json::json!({
            "header": "",
            "question": question,
            "selected": selected,
        }));
        i += 4;
    }
    if answers.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "answers": answers, "declined": false }))
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Peel Grok's `use_tool` MCP envelope (`{tool_name, tool_input}`) into its inner
/// `(tool_name, tool_input)`. Mirrors `connection.rs::unwrap_grok_use_tool` so the
/// history and live paths classify Grok's MCP calls identically. Native tools
/// (args at the top level, no such shape) return `None`.
fn unwrap_use_tool(raw_input: Option<&Value>) -> Option<(&str, &Value)> {
    let obj = raw_input?.as_object()?;
    let tool_name = obj
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let tool_input = obj.get("tool_input")?;
    Some((tool_name, tool_input))
}

/// Extract the readable text from a Grok MCP `rawOutput`
/// (`{"type":"MCP","output":{"OkayOutput":"…"}}`, an `*Output` error variant, or
/// a bare string `output`). Mirrors `connection.rs::grok_mcp_output_text`. The
/// result text is the first string value under `output`. Non-MCP → `None`.
fn grok_mcp_output_text(raw_output: &Value) -> Option<String> {
    if raw_output.get("type").and_then(Value::as_str) != Some("MCP") {
        return None;
    }
    let output = raw_output.get("output")?;
    if let Some(text) = output.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    // First NON-EMPTY string value (the singleton `*Output` variant); filter
    // inside `find_map` so an earlier empty sibling can't shadow a later one.
    output
        .as_object()?
        .values()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// Extract the tool output text from a `tool_call_update`. Prefers the ACP
/// `content[]` array (`{type:"content", content:{type:"text", text}}`), then
/// `rawOutput.output_for_prompt` (Bash/terminal), then a `TaskOutput` envelope
/// (background-task polls — see `grok_task_output_envelope`), then an MCP
/// `rawOutput`'s `output` text (`use_tool`). All are cumulative, so the last
/// update per call carries the full output.
fn update_tool_output(update: &Value) -> Option<String> {
    if let Some(items) = update.get("content").and_then(Value::as_array) {
        let mut buf = String::new();
        for item in items {
            if let Some(text) = item
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
            {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(text);
            }
        }
        if !buf.is_empty() {
            return Some(truncate_str(&buf, GROK_TOOL_OUTPUT_CAP));
        }
    }
    if let Some(text) = update
        .get("rawOutput")
        .and_then(|r| r.get("output_for_prompt"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(truncate_str(text, GROK_TOOL_OUTPUT_CAP));
    }
    if let Some(envelope) = update.get("rawOutput").and_then(grok_task_output_envelope) {
        return Some(envelope);
    }
    update
        .get("rawOutput")
        .and_then(grok_mcp_output_text)
        .map(|s| truncate_str(&s, GROK_TOOL_OUTPUT_CAP))
}

fn tool_input_preview(raw: Option<&Value>) -> Option<String> {
    let raw = raw?;
    if raw.is_null() {
        return None;
    }
    let serialized = serde_json::to_string(raw).ok()?;
    Some(truncate_str(&serialized, GROK_TOOL_INPUT_CAP))
}

/// Serialize an unwrapped MCP `tool_input` for storage as VALID JSON bounded by
/// `GROK_TOOL_INPUT_CAP`. The frontend delegation card `JSON.parse`s this to
/// recover the task/agent_type, so — unlike `tool_input_preview`'s opaque byte
/// truncation, which can corrupt a long-task prompt into unparseable JSON — this
/// truncates the string VALUES (preserving structure) and shrinks the per-string
/// cap until the WHOLE serialized preview also fits the budget. Checking the
/// actual serialized byte length each pass is what bounds every bloat vector
/// (many strings, long arrays, and JSON/UTF-8 escaping that expands bytes),
/// which a single per-field cap could not. Converges in O(log cap) passes; the
/// common (already-small) input returns on the first pass unchanged.
fn grok_mcp_input_preview(input: &Value) -> Option<String> {
    if input.is_null() {
        return None;
    }
    cap_json_to_budget(input, GROK_TOOL_INPUT_CAP)
}

/// Serialize `value` as JSON that stays VALID within `budget` bytes: cap every
/// string value, halving the per-string cap until the WHOLE serialized form
/// fits. Checking the actual serialized length each pass is what bounds every
/// bloat vector (many strings, long arrays, JSON/UTF-8 escaping that expands
/// bytes) — a single per-field cap could not. Converges in O(log budget) passes;
/// an already-small value returns on the first pass unchanged.
/// `pub(crate)`: the DeepSeek parser bounds its oversized tool arguments with
/// the same valid-JSON guarantee (`deepseek_tool_input_preview`).
pub(crate) fn cap_json_to_budget(value: &Value, budget: usize) -> Option<String> {
    let mut per_string = budget;
    loop {
        let serialized = serde_json::to_string(&cap_json_string_values(value, per_string)).ok()?;
        if serialized.len() <= budget || per_string == 0 {
            return Some(serialized);
        }
        per_string /= 2;
    }
}

/// Serialize a Grok `TaskOutput` `rawOutput` — the result of a
/// `get_command_or_subagent_output` poll — for the frontend, which parses it
/// into a background-task card (`@/lib/background-task`). Returns `None` for
/// every other `rawOutput`, so the caller falls through to its normal paths.
///
/// The WHOLE envelope is passed through verbatim (bounded by
/// [`GROK_TASK_OUTPUT_CAP`]): its `type` discriminator is what lets the frontend
/// claim it without hijacking other JSON tool output, and passing it whole means
/// the variants Grok can put beside it (`Result`, `MultiResult`, `TaskNotFound`)
/// need no per-variant handling here. Without this the readable output — the
/// command, exit code and shell text all live under `Result` — is dropped
/// entirely: `content[]` is absent on these updates, and the `output_for_prompt`
/// / MCP paths don't match.
///
/// Shared with the live path (`acp::connection::grok_live_tool_output`) so both
/// hand the frontend a byte-identical string.
pub(crate) fn grok_task_output_envelope(raw_output: &Value) -> Option<String> {
    if raw_output.get("type").and_then(Value::as_str) != Some("TaskOutput") {
        return None;
    }
    cap_json_to_budget(raw_output, GROK_TASK_OUTPUT_CAP)
}

/// Truncate every string value in a JSON value to `cap` chars, preserving
/// structure so the result re-serializes to valid JSON.
fn cap_json_string_values(value: &Value, cap: usize) -> Value {
    match value {
        Value::String(s) => Value::String(truncate_str(s, cap)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| cap_json_string_values(v, cap))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_json_string_values(v, cap)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Update the `ToolResult` block correlated to `id` in the current turn. Grok's
/// `tool_call_update.content` is cumulative, and callers only pass `Some` output
/// when non-empty, so the last non-empty output wins; `failed` only ever sets
/// the error flag (never clears it).
fn apply_tool_result(
    turn: Option<&mut MessageTurn>,
    tool_result_idx: &std::collections::HashMap<String, usize>,
    id: &str,
    output: Option<String>,
    failed: bool,
) {
    let Some(turn) = turn else { return };
    let Some(&idx) = tool_result_idx.get(id) else {
        return;
    };
    if let Some(ContentBlock::ToolResult {
        output_preview,
        is_error,
        ..
    }) = turn.blocks.get_mut(idx)
    {
        if let Some(text) = output {
            *output_preview = Some(text);
        }
        if failed {
            *is_error = true;
        }
    }
}

/// Per-turn stats accumulated from Grok's metadata and applied to the assistant
/// turn at its boundary. Grok exposes the numbers the message footer needs, but
/// in three sibling places the update loop otherwise ignores: context occupancy
/// and timing in the OUTER `params._meta` (`totalTokens`, `turnStartMs`,
/// `agentTimestampMs`), the model in `params.update._meta.modelId`, and the real
/// token split in `turn_completed`'s `update.usage` (see [`prompt_usage`]).
/// Duration is `end - start` in ms.
#[derive(Default)]
struct GrokTurnMeta {
    /// `params._meta.totalTokens` — the context-window OCCUPANCY, not a spend.
    /// It rides nearly every update and feeds the context ring; it is
    /// deliberately never folded into `usage`, which would bill a prompt's
    /// resident context as if it were freshly consumed input.
    total_tokens: Option<u64>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    model: Option<String>,
    /// The prompt's real token spend, stated once by `turn_completed`. `None`
    /// for a turn that never completed.
    usage: Option<TurnUsage>,
}

impl GrokTurnMeta {
    /// Fold one update's metadata in. `params_meta` is `params._meta` (token
    /// total + timing); `update_meta` is `params.update._meta` (carries
    /// `modelId`). `totalTokens` climbs as the turn fills the window, so keep
    /// the max; `turnStartMs` is constant per turn (keep the min defensively);
    /// `agentTimestampMs` advances (keep the max as the turn end).
    fn observe(&mut self, params_meta: Option<&Value>, update_meta: Option<&Value>) {
        if let Some(pm) = params_meta {
            if let Some(tt) = pm.get("totalTokens").and_then(Value::as_u64) {
                self.total_tokens = Some(self.total_tokens.map_or(tt, |cur| cur.max(tt)));
            }
            if let Some(s) = pm.get("turnStartMs").and_then(Value::as_i64) {
                self.start_ms = Some(self.start_ms.map_or(s, |cur| cur.min(s)));
            }
            if let Some(e) = pm.get("agentTimestampMs").and_then(Value::as_i64) {
                self.end_ms = Some(self.end_ms.map_or(e, |cur| cur.max(e)));
            }
        }
        if self.model.is_none() {
            self.model = update_meta
                .and_then(|m| m.get("modelId"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
    }

    /// Apply the accumulated stats to a finalized assistant turn, publishing the
    /// turn's context occupancy into the session-level `context_tokens` (the last
    /// turn to state one wins — that is the session's occupancy now). Never
    /// overwrites a turn field already set.
    fn apply(&self, turn: &mut MessageTurn, context_tokens: &mut Option<u64>) {
        if turn.model.is_none() {
            if let Some(model) = &self.model {
                turn.model = Some(model.clone());
            }
        }
        if let Some(tt) = self.total_tokens.filter(|t| *t > 0) {
            *context_tokens = Some(tt);
        }
        if turn.usage.is_none() {
            if let Some(usage) = &self.usage {
                turn.usage = Some(usage.clone());
            }
        }
        if turn.duration_ms.is_none() {
            if let (Some(start), Some(end)) = (self.start_ms, self.end_ms) {
                if end > start {
                    turn.duration_ms = Some((end - start) as u64);
                }
            }
        }
    }
}

/// Read the `usage` object Grok attaches to `turn_completed` into disjoint
/// `TurnUsage` buckets. `None` when it states nothing at all (every counter
/// absent or zero).
///
/// The figures are PER PROMPT, not running session totals — each
/// `turn_completed` carries its own `prompt_id` and, per Grok's docs, "`usage`
/// sums tokens for the prompt, including subagents that finished before turn
/// end" (`~/.grok/docs/user-guide/14-headless-mode.md`). So the snapshot is
/// attached to its own turn as-is and `compute_session_stats` sums the prompts.
/// The counters do climb from one `turn_completed` to the next in a captured
/// session, which reads as cumulative at a glance; it isn't. In `019f96d5` the
/// second prompt states 198457 input over 7 `modelCalls` — a shade under 28.4K
/// per call against that session's 31628-token peak occupancy — whereas reading
/// the pair as cumulative would charge its 2 additional calls 112283 tokens
/// (56K per call), which no call could have sent through a window that size.
///
/// Bucket semantics: this is the ACP `PromptUsage` flavour, whose `inputTokens`
/// is the WHOLE prompt side, with `cachedReadTokens` / `cacheCreationTokens`
/// naming cached SLICES OF IT rather than separate addends —
/// `inputTokens + outputTokens == totalTokens` holds in every capture, which it
/// could not if the cache counters sat outside `inputTokens`. `TurnUsage`
/// follows Anthropic's DISJOINT convention, where `input_tokens` excludes both
/// cache buckets (see `parsers::claude`), so the cached slices are subtracted
/// back out; the four buckets then re-sum to Grok's own `totalTokens`, which is
/// what the composer's breakdown adds up for its "total" row.
/// `reasoningTokens` is a subset of `outputTokens` and so is deliberately
/// dropped, mirroring `parsers::deepseek`.
///
/// Only the ACP spellings are read. Grok's headless projector publishes the same
/// quantities under `cacheReadInputTokens` / `input_tokens`, but with the cache
/// ALREADY subtracted out ("`usage.input_tokens` … are **uncached only**" —
/// same doc). Aliasing those names in here would silently double-subtract the
/// cached prefix the day a build emits them.
fn prompt_usage(usage: &Value) -> Option<TurnUsage> {
    let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = field("inputTokens");
    let output = field("outputTokens");
    // `cacheCreationTokens` reaches only newer Grok builds; older ones report
    // reads alone, and an absent counter reads as 0 either way.
    let cache_read = field("cachedReadTokens");
    let cache_create = field("cacheCreationTokens");
    if input == 0 && output == 0 && cache_read == 0 && cache_create == 0 {
        return None;
    }
    Some(TurnUsage {
        // Saturating rather than wrapping: a build that ever reports cache
        // counters exceeding `inputTokens` keeps its cache buckets and drops the
        // uncached remainder to zero, instead of inventing a colossal input.
        input_tokens: input
            .saturating_sub(cache_read)
            .saturating_sub(cache_create),
        output_tokens: output,
        cache_creation_input_tokens: cache_create,
        cache_read_input_tokens: cache_read,
    })
}

fn ensure_assistant<'a>(
    assistant: &'a mut Option<MessageTurn>,
    ts: DateTime<Utc>,
    session_id: &str,
    pending: &mut Option<PendingGrokAutonomous>,
    active_autonomous: &mut Option<PendingGrokAutonomous>,
) -> &'a mut MessageTurn {
    if assistant.is_none() {
        let (id, origin) = match pending.take() {
            Some(p) => {
                let id = grok_autonomous_turn_id(session_id, &p.task_ids, p.trigger_start);
                *active_autonomous = Some(p);
                (id, Some(AutonomousTurnOrigin::BackgroundTask))
            }
            None => (String::new(), None),
        };
        *assistant = Some(MessageTurn {
            id,
            role: TurnRole::Assistant,
            blocks: Vec::new(),
            timestamp: ts,
            usage: None,
            duration_ms: None,
            model: None,
            reasoning_effort: None,
            completed_at: None,
            outcome: None,
            autonomous_origin: origin,
            generation_ms: None,
            generation_tokens: None,
        });
    }
    assistant.as_mut().expect("assistant just set")
}

fn flush_assistant(
    assistant: &mut Option<MessageTurn>,
    turns: &mut Vec<MessageTurn>,
    tool_result_idx: &mut std::collections::HashMap<String, usize>,
) {
    if let Some(turn) = assistant.take() {
        turns.push(turn);
    }
    tool_result_idx.clear();
}

/// Append assistant text, merging into the trailing `Text` block when adjacent
/// (streaming deltas concatenate; distinct segments separated by tools/thoughts
/// stay separate blocks).
fn append_text(turn: &mut MessageTurn, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(ContentBlock::Text { text: last }) = turn.blocks.last_mut() {
        last.push_str(&text);
    } else {
        turn.blocks.push(ContentBlock::Text { text });
    }
}

fn append_thinking(turn: &mut MessageTurn, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(ContentBlock::Thinking { text: last }) = turn.blocks.last_mut() {
        last.push('\n');
        last.push_str(&text);
    } else {
        turn.blocks.push(ContentBlock::Thinking { text });
    }
}

/// Immediate subdirectories of `dir` (non-recursive). Missing dir → empty.
fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message::AutonomousTurnOrigin;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};

    /// Serialize tests that mutate `GROK_HOME` so parallel cargo workers do not
    /// race on the process environment.
    fn with_temp_grok_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK.get_or_init(|| Mutex::new(()));
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        struct Restore(Option<OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("GROK_HOME", v),
                    None => std::env::remove_var("GROK_HOME"),
                }
            }
        }
        let _restore = Restore(std::env::var_os("GROK_HOME"));
        std::env::set_var("GROK_HOME", home);
        f()
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        let mut f = fs::File::create(dir.join(name)).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    /// Build a `sessions/<group>/<uuid>/` fixture with the given summary +
    /// updates, returning the base `sessions/` dir.
    fn fixture(summary: &str, updates: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let session = sessions
            .join("%2FUsers%2Fme%2Fproj")
            .join("019f45e3-e1ef-7690-a29f-fe2554382b49");
        fs::create_dir_all(&session).unwrap();
        write(&session, "summary.json", summary);
        write(&session, "updates.jsonl", updates);
        (tmp, sessions)
    }

    #[test]
    fn legacy_completion_reminders_only_accept_verified_bash_and_monitor_shapes() {
        let bash = concat!(
            "<system-reminder>\n",
            "Background task \"term_x\" completed (exit code: 0).\n",
            "Command: pnpm build | Duration: 1.0s\n",
            "</system-reminder>",
        );
        let monitor = concat!(
            "<system-reminder>\n",
            "Monitor \"monitor-1\" ended: [monitor ended: exited (code 0)].\n",
            "Description: build\n",
            "</system-reminder>",
        );

        assert_eq!(grok_reminder_task_ids(bash), vec!["term_x"]);
        assert_eq!(grok_reminder_task_ids(monitor), vec!["monitor-1"]);
        assert!(is_grok_background_task_reminder(bash));
        assert!(is_grok_background_task_reminder(monitor));
        assert!(!is_grok_background_task_reminder(
            "<system-reminder>Background task text changed</system-reminder>"
        ));
        assert!(!is_grok_background_task_reminder(
            "Background task \"term_x\" completed (exit code: 0)."
        ));
    }

    #[test]
    fn injected_locator_prefers_one_strict_match_over_loose_matches() {
        let root = tempfile::tempdir().unwrap();
        let strict = root.path().join("a/session-1");
        let loose = root.path().join("b/session-1");
        fs::create_dir_all(&strict).unwrap();
        fs::create_dir_all(&loose).unwrap();
        fs::write(strict.join("updates.jsonl"), b"\n").unwrap();
        assert_eq!(
            locate_grok_session_dir(root.path(), "session-1").unwrap(),
            Some(strict)
        );
    }

    #[test]
    fn injected_locator_uses_one_loose_match_when_no_strict_match_exists() {
        let root = tempfile::tempdir().unwrap();
        let loose = root.path().join("a/session-1");
        fs::create_dir_all(&loose).unwrap();
        assert_eq!(
            locate_grok_session_dir(root.path(), "session-1").unwrap(),
            Some(loose)
        );
    }

    #[test]
    fn missing_sessions_root_is_absent() {
        let root = tempfile::tempdir().unwrap().path().join("gone");
        assert_eq!(locate_grok_session_dir(&root, "session-1").unwrap(), None);
    }

    #[test]
    fn duplicate_strict_matches_are_rejected_deterministically() {
        let root = tempfile::tempdir().unwrap();
        for group in ["z", "a"] {
            let dir = root.path().join(group).join("session-1");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("updates.jsonl"), b"\n").unwrap();
        }
        assert!(matches!(
            locate_grok_session_dir(root.path(), "session-1"),
            Err(GrokSessionLocatorError::Ambiguous {
                strictness: GrokSessionMatchStrictness::Strict,
                count: 2,
            })
        ));
    }

    #[test]
    fn duplicate_loose_matches_are_rejected_deterministically() {
        let root = tempfile::tempdir().unwrap();
        for group in ["z", "a"] {
            fs::create_dir_all(root.path().join(group).join("session-1")).unwrap();
        }
        assert!(matches!(
            locate_grok_session_dir(root.path(), "session-1"),
            Err(GrokSessionLocatorError::Ambiguous {
                strictness: GrokSessionMatchStrictness::Loose,
                count: 2,
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_sessions_root_preserves_permission_error() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            permissions: fs::Permissions,
        }

        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
            }
        }

        let root = tempfile::tempdir().unwrap();
        let original = fs::metadata(root.path()).unwrap().permissions();
        let _restore = RestorePermissions {
            path: root.path().to_path_buf(),
            permissions: original,
        };
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o000)).unwrap();
        let probe = fs::read_dir(root.path()).unwrap_err();
        if probe.kind() != std::io::ErrorKind::PermissionDenied {
            return;
        }

        assert!(matches!(
            locate_grok_session_dir(root.path(), "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_group_preserves_permission_error() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            permissions: fs::Permissions,
        }

        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
            }
        }

        let root = tempfile::tempdir().unwrap();
        let group = root.path().join("a");
        let candidate = group.join("session-1");
        fs::create_dir_all(&candidate).unwrap();
        let original = fs::metadata(&group).unwrap().permissions();
        let _restore = RestorePermissions {
            path: group.clone(),
            permissions: original,
        };
        fs::set_permissions(&group, fs::Permissions::from_mode(0o000)).unwrap();
        let probe = fs::symlink_metadata(&candidate).unwrap_err();
        if probe.kind() != std::io::ErrorKind::PermissionDenied {
            return;
        }

        assert!(matches!(
            locate_grok_session_dir(root.path(), "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_updates_metadata_preserves_permission_error() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            permissions: fs::Permissions,
        }

        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
            }
        }

        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("a/session-1");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("updates.jsonl"), b"\n").unwrap();
        let original = fs::metadata(&session).unwrap().permissions();
        let _restore = RestorePermissions {
            path: session.clone(),
            permissions: original,
        };
        fs::set_permissions(&session, fs::Permissions::from_mode(0o000)).unwrap();
        let probe = fs::metadata(session.join("updates.jsonl")).unwrap_err();
        if probe.kind() != std::io::ErrorKind::PermissionDenied {
            return;
        }

        assert!(matches!(
            locate_grok_session_dir(root.path(), "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_session_identity_is_returned_for_authority_validation() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let target = external.path().join("session-1");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("updates.jsonl"), b"\n").unwrap();
        let alias = root.path().join("a/session-1");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink(&target, &alias).unwrap();

        assert_eq!(
            locate_grok_session_dir(root.path(), "session-1").unwrap(),
            Some(alias)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_group_identity_is_returned_for_authority_validation() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let target = external.path().join("session-1");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("updates.jsonl"), b"\n").unwrap();
        let group_alias = root.path().join("a");
        symlink(external.path(), &group_alias).unwrap();
        let alias = group_alias.join("session-1");

        assert_eq!(
            locate_grok_session_dir(root.path(), "session-1").unwrap(),
            Some(alias)
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_sessions_root_is_an_error_not_absent() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        symlink(root.path().join("missing"), &sessions).unwrap();

        assert!(matches!(
            locate_grok_session_dir(&sessions, "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_sessions_root_ancestor_is_an_error_not_absent() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dangling_ancestor = root.path().join("grok");
        symlink(root.path().join("missing-grok"), &dangling_ancestor).unwrap();
        let sessions = dangling_ancestor.join("sessions");

        assert!(matches!(
            locate_grok_session_dir(&sessions, "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[cfg(windows)]
    #[test]
    fn dangling_sessions_root_is_an_error_not_absent() {
        use std::os::windows::fs::symlink_dir;

        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        if symlink_dir(root.path().join("missing"), &sessions).is_err() {
            return;
        }

        assert!(matches!(
            locate_grok_session_dir(&sessions, "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[cfg(windows)]
    #[test]
    fn dangling_sessions_root_ancestor_is_an_error_not_absent() {
        use std::os::windows::fs::symlink_dir;

        let root = tempfile::tempdir().unwrap();
        let dangling_ancestor = root.path().join("grok");
        if symlink_dir(root.path().join("missing-grok"), &dangling_ancestor).is_err() {
            return;
        }
        let sessions = dangling_ancestor.join("sessions");

        assert!(matches!(
            locate_grok_session_dir(&sessions, "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn drive_relative_root_shape_is_terminal() {
        assert!(is_unsupported_drive_relative_root_shape(false, true));
        assert!(!is_unsupported_drive_relative_root_shape(true, true));
        assert!(!is_unsupported_drive_relative_root_shape(false, false));
    }

    #[cfg(windows)]
    #[test]
    fn drive_relative_sessions_root_is_rejected_before_fallback() {
        assert!(matches!(
            locate_grok_session_dir(Path::new(r"C:grok\sessions"), "session-1"),
            Err(GrokSessionLocatorError::Io(error))
                if error.kind() == std::io::ErrorKind::InvalidInput
        ));
    }

    const SUMMARY: &str = r#"{
        "info": {"id": "019f45e3-e1ef-7690-a29f-fe2554382b49", "cwd": "/Users/me/proj"},
        "session_summary": "Fallback summary",
        "generated_title": "Build the project",
        "created_at": "2026-07-09T07:59:50.598122Z",
        "updated_at": "2026-07-09T08:02:09.789572Z",
        "num_messages": 6,
        "current_model_id": "grok-4.5",
        "head_branch": "main"
    }"#;

    // Two turns: a plain Q&A, then a prompt that runs a backgrounded command.
    const UPDATES: &str = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"你会做什么"},"_meta":{"modelId":"grok-4.5","promptIndex":0}}},"timestamp":1783584019}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Thinking about it"}}},"timestamp":1783584019}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"我是 Grok"}}},"timestamp":1783584024}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p0","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"执行 pnpm build"},"_meta":{"promptIndex":1}}},"timestamp":1783584029}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"正在执行"}}},"timestamp":1783584029}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-1","title":"run_terminal_command","rawInput":{"command":"pnpm build","background":true},"_meta":{"x.ai/tool":{"name":"run_terminal_command","kind":"execute"}}}},"timestamp":1783584029}"#,
        "\n",
        // The only event pairing the task id with the launching tool call.
        r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_backgrounded","tool_call_id":"call-1","task_id":"term_x","command":"pnpm build"}},"timestamp":1783584029}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed","title":"[bg] pnpm build (term_x)","content":[{"type":"content","content":{"type":"text","text":"Background task term_x started"}}]}},"timestamp":1783584033}"#,
        "\n",
        // Snapshot keyed by `task_id` — deliberately ignored, so the launch call
        // stays exactly as the wire (and the live path) reports it.
        r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_completed","task_snapshot":{"task_id":"term_x","command":"/bin/bash -lc 'pnpm build'","output":"boom","exit_code":1}}},"timestamp":1783584122}"#,
        "\n",
        // The model polls the task; its whole result lives in `rawOutput`.
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-2","title":"get_command_or_subagent_output","rawInput":{"task_ids":["term_x"],"timeout_ms":15000},"_meta":{"x.ai/tool":{"name":"get_command_or_subagent_output","kind":"background_task_action"}}}},"timestamp":1783584123}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-2","status":"completed","title":"/bin/bash -lc 'pnpm build' (term_x)","rawOutput":{"type":"TaskOutput","Result":{"task_id":"term_x","command":"/bin/bash -lc 'pnpm build'","status":"failed","exit_code":1,"output":"boom"}}}},"timestamp":1783584124}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn"}},"timestamp":1783584129}"#,
        "\n",
    );

    #[test]
    fn lists_session_with_metadata() {
        let (_tmp, sessions) = fixture(SUMMARY, UPDATES);
        let parser = GrokParser::with_base_dir(sessions);
        let list = parser.list_conversations().unwrap();
        assert_eq!(list.len(), 1);
        let s = &list[0];
        assert_eq!(s.id, "019f45e3-e1ef-7690-a29f-fe2554382b49");
        assert_eq!(s.agent_type, AgentType::Grok);
        assert_eq!(s.title.as_deref(), Some("Build the project"));
        assert_eq!(s.model.as_deref(), Some("grok-4.5"));
        assert_eq!(s.folder_path.as_deref(), Some("/Users/me/proj"));
        assert_eq!(s.git_branch.as_deref(), Some("main"));
        // 2 user + 2 assistant turns.
        assert_eq!(s.message_count, 4);
    }

    #[test]
    fn parses_turns_blocks_and_tool_result() {
        let (_tmp, sessions) = fixture(SUMMARY, UPDATES);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let turns = &detail.turns;
        assert_eq!(turns.len(), 4);

        assert!(matches!(turns[0].role, TurnRole::User));
        assert!(matches!(&turns[0].blocks[0], ContentBlock::Text { text } if text == "你会做什么"));

        assert!(matches!(turns[1].role, TurnRole::Assistant));
        assert!(
            matches!(&turns[1].blocks[0], ContentBlock::Thinking { text } if text == "Thinking about it")
        );
        assert!(matches!(&turns[1].blocks[1], ContentBlock::Text { text } if text == "我是 Grok"));

        // Assistant turn 2: text, then tool use + tool result.
        let last = &turns[3];
        assert!(matches!(last.role, TurnRole::Assistant));
        let tool_use = last
            .blocks
            .iter()
            .find(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .unwrap();
        assert!(
            matches!(tool_use, ContentBlock::ToolUse { tool_name, .. } if tool_name == "run_terminal_command")
        );
        // The launch keeps its own "started" text and its wire status: Grok
        // reports the CALL as completed (it did start the task), and the failing
        // `task_completed` snapshot is not applied — otherwise history would
        // contradict the live path, which never sees that ext notification. The
        // task's failure surfaces on the poll below.
        let tool_result = last
            .blocks
            .iter()
            .find(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .unwrap();
        assert!(matches!(
            tool_result,
            ContentBlock::ToolResult { output_preview, is_error, .. }
                if output_preview.as_deref() == Some("Background task term_x started") && !*is_error
        ));
    }

    /// A `get_command_or_subagent_output` poll carries its whole result in
    /// `rawOutput` (no `content[]`, no `output_for_prompt`), which used to be
    /// dropped — leaving the card empty. It must reach the frontend verbatim so
    /// the background-task card can render command/status/exit code/output.
    #[test]
    fn background_task_poll_surfaces_task_output_envelope() {
        let (_tmp, sessions) = fixture(SUMMARY, UPDATES);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let blocks = &detail.turns[3].blocks;

        let poll = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse {
                    tool_name,
                    tool_use_id,
                    ..
                } => Some((tool_name, tool_use_id)),
                _ => None,
            })
            .find(|(name, _)| name.as_str() == "get_command_or_subagent_output")
            .expect("poll tool use");
        assert_eq!(poll.1.as_deref(), Some("call-2"));

        let output = blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    output_preview,
                    ..
                } if tool_use_id.as_deref() == Some("call-2") => output_preview.clone(),
                _ => None,
            })
            .expect("poll ToolResult output");
        let env: Value = serde_json::from_str(&output).expect("envelope is valid JSON");
        assert_eq!(env["type"], "TaskOutput");
        assert_eq!(env["Result"]["exit_code"], 1);
        assert_eq!(env["Result"]["status"], "failed");
        assert_eq!(env["Result"]["output"], "boom");
    }

    /// Grok injects reminders as `user_message_chunk`s flagged
    /// `_meta.hideFromScrollback`. Rendering them as user bubbles split one reply
    /// into two turns with a raw `<system-reminder>` wedged between.
    #[test]
    fn hidden_user_chunk_does_not_split_the_reply() {
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"启动服务"},"_meta":{"modelId":"grok-4.5","promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"已启动"}}},"timestamp":1783584020}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p0","stop_reason":"end_turn"}},"timestamp":1783584021}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 1).\n</system-reminder>"},"_meta":{"modelId":"grok-4.5","promptIndex":1,"hideFromScrollback":true}}},"timestamp":1783584022}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"那次失败可以忽略"}}},"timestamp":1783584023}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();

        // One real prompt + the two assistant replies; no reminder bubble.
        assert_eq!(
            detail
                .turns
                .iter()
                .filter(|t| matches!(t.role, TurnRole::User))
                .count(),
            1
        );
        assert!(!detail.turns.iter().any(|t| t.blocks.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.contains("system-reminder"))
        )));
        assert!(
            matches!(&detail.turns[2].blocks[0], ContentBlock::Text { text } if text == "那次失败可以忽略")
        );
    }

    #[test]
    fn updates_watermark_is_complete_line_bytes_only() {
        let complete = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
        );
        let partial = r#"{"method":"session/update""#;
        let (_tmp, sessions) = fixture(SUMMARY, &format!("{complete}{partial}"));
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        assert_eq!(detail.transcript_watermark, Some(complete.len() as u64));
    }

    #[test]
    fn idle_hidden_trigger_marks_following_assistant_background_task() {
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"started"}}},"timestamp":2}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":3}"#,
            "\n",
            r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_completed","task_id":"term_x"}},"timestamp":4}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},"_meta":{"hideFromScrollback":true,"promptIndex":1}}},"timestamp":5}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}},"timestamp":6}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let sessions_again = sessions.clone();
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        assert_eq!(
            detail
                .turns
                .iter()
                .filter(|t| matches!(t.role, TurnRole::User))
                .count(),
            1
        );
        assert!(!detail.turns.iter().any(|t| t.blocks.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.contains("system-reminder"))
        )));
        let auto = detail
            .turns
            .iter()
            .find(|t| t.autonomous_origin == Some(AutonomousTurnOrigin::BackgroundTask))
            .expect("autonomous assistant");
        assert!(auto.id.starts_with("grok-autonomous:"));
        assert!(auto.id.ends_with(":assistant:0"));
        let trigger_prefix = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"started"}}},"timestamp":2}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":3}"#,
            "\n",
            r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_completed","task_id":"term_x"}},"timestamp":4}"#,
            "\n",
        );
        assert_eq!(
            auto.id,
            grok_autonomous_turn_id(
                "019f45e3-e1ef-7690-a29f-fe2554382b49",
                &["term_x".to_string()],
                trigger_prefix.len() as u64,
            )
        );
        assert!(matches!(&auto.blocks[0], ContentBlock::Text { text } if text == "done"));
        let detail2 = GrokParser::with_base_dir(sessions_again)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let auto2 = detail2
            .turns
            .iter()
            .find(|t| t.autonomous_origin.is_some())
            .unwrap();
        assert_eq!(auto.id, auto2.id);
    }

    #[test]
    fn hidden_reminder_inside_open_assistant_does_not_relabel_it() {
        // Same reminder body as `hidden_user_chunk_does_not_split_the_reply`,
        // injected while the first assistant is still open (no `turn_completed`).
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"启动服务"},"_meta":{"modelId":"grok-4.5","promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"已启动"}}},"timestamp":1783584020}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 1).\n</system-reminder>"},"_meta":{"modelId":"grok-4.5","promptIndex":1,"hideFromScrollback":true}}},"timestamp":1783584022}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"那次失败可以忽略"}}},"timestamp":1783584023}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        assert_eq!(
            detail
                .turns
                .iter()
                .filter(|t| matches!(t.role, TurnRole::User))
                .count(),
            1
        );
        assert!(!detail.turns.iter().any(|t| t.blocks.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.contains("system-reminder"))
        )));
        assert_eq!(
            detail
                .turns
                .iter()
                .filter(|t| matches!(t.role, TurnRole::Assistant))
                .count(),
            1
        );
        assert!(
            matches!(&detail.turns[1].blocks[0], ContentBlock::Text { text } if text == "已启动那次失败可以忽略")
        );
        assert!(detail.turns.iter().all(|t| t.autonomous_origin.is_none()));
        assert!(detail.turns[1].id.starts_with("grok-turn-"));
    }

    #[test]
    fn two_triggers_same_task_get_distinct_ids() {
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"started"}}},"timestamp":2}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":3}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},"_meta":{"hideFromScrollback":true}}},"timestamp":4}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first"}}},"timestamp":5}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":6}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},"_meta":{"hideFromScrollback":true}}},"timestamp":7}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second"}}},"timestamp":8}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let autos: Vec<&MessageTurn> = detail
            .turns
            .iter()
            .filter(|t| t.autonomous_origin == Some(AutonomousTurnOrigin::BackgroundTask))
            .collect();
        assert_eq!(autos.len(), 2);
        assert!(matches!(&autos[0].blocks[0], ContentBlock::Text { text } if text == "first"));
        assert!(matches!(&autos[1].blocks[0], ContentBlock::Text { text } if text == "second"));
        assert_ne!(autos[0].id, autos[1].id);
        assert!(autos[0].id.starts_with("grok-autonomous:"));
        assert!(autos[1].id.starts_with("grok-autonomous:"));
        assert!(autos[0].id.ends_with(":assistant:0"));
        assert!(autos[1].id.ends_with(":assistant:0"));
    }

    #[test]
    fn grok_autonomous_turn_id_documents_episode_key() {
        assert_eq!(
            grok_autonomous_turn_id(
                "019f45e3-e1ef-7690-a29f-fe2554382b49",
                &["term_x".to_string()],
                42,
            ),
            "grok-autonomous:019f45e3-e1ef-7690-a29f-fe2554382b49+term_x+42:assistant:0"
        );
        assert_eq!(
            grok_autonomous_turn_id("sess", &[], 7),
            "grok-autonomous:sess+7:assistant:0"
        );
        let digested = grok_autonomous_turn_id("sess", &["a:b".to_string()], 1);
        let key = digested
            .strip_prefix("grok-autonomous:")
            .and_then(|s| s.strip_suffix(":assistant:0"))
            .unwrap();
        assert_eq!(key.len(), 64);
        assert!(key
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(
            digested,
            grok_autonomous_turn_id("sess", &["a:b".to_string()], 1)
        );
    }

    #[test]
    fn malformed_and_non_utf8_complete_lines_count_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let session = sessions
            .join("%2FUsers%2Fme%2Fproj")
            .join("019f45e3-e1ef-7690-a29f-fe2554382b49");
        fs::create_dir_all(&session).unwrap();
        write(&session, "summary.json", SUMMARY);
        let good = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(good.as_bytes());
        bytes.extend_from_slice(b"not-json\n");
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        fs::write(session.join("updates.jsonl"), &bytes).unwrap();
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        assert_eq!(detail.transcript_watermark, Some(bytes.len() as u64));
        assert_eq!(
            detail
                .turns
                .iter()
                .filter(|t| matches!(t.role, TurnRole::User))
                .count(),
            1
        );
    }

    #[test]
    fn hidden_non_reminder_does_not_mark_origin() {
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"started"}}},"timestamp":2}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":3}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"please continue the previous work"},"_meta":{"hideFromScrollback":true}}},"timestamp":4}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ok"}}},"timestamp":5}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        assert!(detail.turns.iter().all(|t| t.autonomous_origin.is_none()));
        assert!(detail.turns.iter().all(|t| t.id.starts_with("grok-turn-")));
    }

    #[test]
    fn hidden_trigger_after_committed_user_does_not_mark_that_assistant() {
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"please continue"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},"_meta":{"hideFromScrollback":true}}},"timestamp":2}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"user reply"}}},"timestamp":3}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail
            .turns
            .iter()
            .find(|t| matches!(t.role, TurnRole::Assistant))
            .expect("assistant");
        assert!(assistant.autonomous_origin.is_none());
        assert!(assistant.id.starts_with("grok-turn-"));
    }

    #[test]
    fn history_renders_auto_compaction_as_context_compaction_tool() {
        // Grok's auto-compaction lands on the namespaced `_x.ai/session/update`
        // method as `auto_compact_completed` (real capture, session 019f9432:
        // 51777 → 4616 tokens), preceded by a `compaction_checkpoint` (ignored).
        // History must surface the outcome as a completed ToolUse tagged
        // `meta.contextCompaction` with the token delta — mirroring the live path —
        // rather than dropping it.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"plan a page"},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ok"}}},"timestamp":1783584020}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584021}"#,
            "\n",
            r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"compaction_checkpoint","checkpoint_id":"c1","prompt_index_at_compaction":0},"_meta":{"eventId":"ev-compact-1"}},"timestamp":1783584030}"#,
            "\n",
            r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"auto_compact_completed","tokens_before":51777,"tokens_after":4616,"summary_preview":null},"_meta":{"eventId":"ev-compact-2"}},"timestamp":1783584030}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":1}}},"timestamp":1783584031}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        // The compaction ToolUse carries the shared card's meta anywhere in the
        // timeline (rendered between the prior turn and the next "hi" prompt).
        let compaction = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolUse { meta: Some(m), .. }
                    if m.get("contextCompaction").and_then(|v| v.as_bool()) == Some(true) =>
                {
                    Some(m.clone())
                }
                _ => None,
            })
            .expect("compaction tool_use present in history");
        assert_eq!(
            compaction.get("tokensBefore").and_then(|v| v.as_u64()),
            Some(51777)
        );
        assert_eq!(
            compaction.get("tokensAfter").and_then(|v| v.as_u64()),
            Some(4616)
        );
        // The paired ToolResult keeps the block well-formed (no orphan tool_use).
        let has_result = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        assert!(has_result, "compaction tool_result present");
    }

    #[test]
    fn merges_prompt_text_and_native_image_into_one_user_turn() {
        // Grok echoes a native ACP image as its own `user_message_chunk` (same
        // `promptIndex` as the prose) — the shape captured from a live 1.0.0 and
        // re-checked on 1.0.3. Same merge rule as the legacy resource-blob shape
        // below.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"这是什么"},"_meta":{"modelId":"grok-4.6","promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"image","data":"QUJD","mimeType":"image/png"},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"一张截图"}}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let turns = &detail.turns;
        assert_eq!(turns.len(), 2);
        assert!(matches!(turns[0].role, TurnRole::User));
        assert_eq!(turns[0].blocks.len(), 2);
        assert!(matches!(&turns[0].blocks[0], ContentBlock::Text { text } if text == "这是什么"));
        assert!(matches!(
            &turns[0].blocks[1],
            ContentBlock::Image { data, mime_type, .. }
                if data == "QUJD" && mime_type == "image/png"
        ));
        assert!(matches!(turns[1].role, TurnRole::Assistant));
    }

    #[test]
    fn merges_prompt_text_and_image_resource_into_one_user_turn() {
        // Older transcripts: Grok echoed a pasted image as an embedded
        // resource blob (`user_message_chunk` after the prose, same
        // `promptIndex`). Both must still land in ONE user turn as
        // [Text, Image] — not a text turn plus a trailing image-only turn.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"这是什么"},"_meta":{"modelId":"grok-4.5","promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"resource","resource":{"blob":"QUJD","mimeType":"image/png","uri":"clipboard://image.png-abc"}},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"一张截图"}}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let turns = &detail.turns;
        // One user turn + one assistant turn — NOT two user turns.
        assert_eq!(turns.len(), 2);
        assert!(matches!(turns[0].role, TurnRole::User));
        assert_eq!(turns[0].blocks.len(), 2);
        assert!(matches!(&turns[0].blocks[0], ContentBlock::Text { text } if text == "这是什么"));
        assert!(matches!(
            &turns[0].blocks[1],
            ContentBlock::Image { data, mime_type, uri }
                if data == "QUJD"
                    && mime_type == "image/png"
                    && uri.as_deref() == Some("clipboard://image.png-abc")
        ));
        assert!(matches!(turns[1].role, TurnRole::Assistant));
    }

    #[test]
    fn parse_context_window_prefers_exact_model_block() {
        // Model ids with dots must be quoted in TOML headers (`[model."grok-4.5"]`),
        // matching how codeg / Grok write config.toml.
        let toml = r#"
[models]
default = "my-proxy"

[model.my-proxy]
model = "my-proxy"
context_window = 131072

[model."grok-4.5"]
model = "grok-4.5"
context_window = 200000
"#;
        assert_eq!(
            parse_grok_model_context_window_from_toml(toml, Some("my-proxy")),
            Some(131_072)
        );
        assert_eq!(
            parse_grok_model_context_window_from_toml(toml, Some("grok-4.5")),
            Some(200_000)
        );
        // Stock model without a block → no configured size (inference later).
        assert_eq!(
            parse_grok_model_context_window_from_toml(toml, Some("grok-4.3")),
            None
        );
        // No model id → fall back to [models].default block.
        assert_eq!(
            parse_grok_model_context_window_from_toml(toml, None),
            Some(131_072)
        );
        assert_eq!(
            resolve_grok_context_window_max_tokens(Some("grok-4.5"), Some(131_072)),
            131_072
        );
        assert_eq!(
            resolve_grok_context_window_max_tokens(Some("grok-4.5"), None),
            500_000
        );
    }

    #[test]
    fn build_detail_uses_configured_context_window_from_grok_home() {
        // Point GROK_HOME at a temp config so we don't read the developer's
        // real ~/.grok/config.toml (which may set a custom context_window).
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "config.toml",
            r#"
[models]
default = "grok-4.5"

[model."grok-4.5"]
model = "grok-4.5"
context_window = 131072
"#,
        );

        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"modelId":"grok-4.5","promptIndex":0}},"_meta":{"totalTokens":1000}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ok"}},"_meta":{"totalTokens":1000}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_sess_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let stats = with_temp_grok_home(tmp.path(), || {
            parser
                .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
                .unwrap()
                .session_stats
                .expect("session stats")
        });
        assert_eq!(stats.context_window_used_tokens, Some(1000));
        // Must honor settings context_window (131072), not model-family 500K.
        assert_eq!(stats.context_window_max_tokens, Some(131_072));
    }
    /// One turn whose stats live where Grok really puts them: model in
    /// `update._meta.modelId`, occupancy `totalTokens` and timing in the OUTER
    /// `params._meta`. Shared by the context-ring tests below.
    const RING_UPDATES: &str = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"modelId":"grok-4.5-fast","promptIndex":0}},"_meta":{"turnStartMs":1000,"totalTokens":100}},"timestamp":1783584019}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}},"_meta":{"totalTokens":500,"agentTimestampMs":3000}},"timestamp":1783584024}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"},"_meta":{"agentTimestampMs":5000}},"timestamp":1783584024}"#,
        "\n",
    );

    #[test]
    fn assistant_turn_carries_model_tokens_and_duration() {
        // Isolate from the developer's real ~/.grok/config.toml so the max
        // comes from model-family inference (grok-4.5 → 500K), not a custom
        // context_window override.
        let empty_home = tempfile::tempdir().unwrap();

        // Grok reports the footer's stats in two sibling metadata places the
        // loop must fold in: model in `update._meta.modelId`, and context
        // occupancy + timing in the OUTER `params._meta` (`totalTokens`,
        // `turnStartMs` → `agentTimestampMs`).
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"modelId":"grok-4.5-fast","promptIndex":0}},"_meta":{"turnStartMs":1000,"totalTokens":100}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}},"_meta":{"totalTokens":500,"agentTimestampMs":3000}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"},"_meta":{"agentTimestampMs":5000}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = with_temp_grok_home(empty_home.path(), || {
            parser
                .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
                .unwrap()
        });
        let assistant = detail.turns.last().expect("assistant turn");
        assert!(matches!(assistant.role, TurnRole::Assistant));
        // In-stream modelId wins over the summary's current_model_id.
        assert_eq!(assistant.model.as_deref(), Some("grok-4.5-fast"));
        // `totalTokens` is occupancy, not spend: it feeds the ring below and
        // must NOT be dressed up as `input_tokens`. This fixture's
        // `turn_completed` states no `usage`, so the turn honestly has none.
        assert!(assistant.usage.is_none());
        // Duration = last agentTimestampMs (5000) − turnStartMs (1000).
        assert_eq!(assistant.duration_ms, Some(4000));

        // Session stats aggregate the turn duration; with no turn stating usage
        // there is no token breakdown to report.
        let stats = detail.session_stats.expect("session stats");
        assert!(stats.total_usage.is_none());
        assert_eq!(stats.total_duration_ms, 4000);
        // Context ring: occupancy (500) as "used", paired with the session
        // model's window (summary current_model_id = grok-4.5 → 500K).
        // Without this the status bar shows no context ring for Grok.
        assert_eq!(stats.context_window_used_tokens, Some(500));
        assert_eq!(stats.context_window_max_tokens, Some(500_000));
        let pct = stats
            .context_window_usage_percent
            .expect("context window percent");
        assert!((pct - 0.1).abs() < 1e-6, "pct = {pct}");
    }

    /// Two prompts, each closed by a `turn_completed` stating that PROMPT's own
    /// `usage`. Numbers (and the distinct `prompt_id`s) lifted verbatim from a
    /// real capture (`~/.grok/sessions/…/019f96d5…/updates.jsonl`), which is what
    /// makes the arithmetic below meaningful rather than self-referential.
    const USAGE_UPDATES: &str = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"one"},"_meta":{"promptIndex":0}},"_meta":{"turnStartMs":1000,"totalTokens":9000}},"timestamp":1783584019}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first"}},"_meta":{"totalTokens":18000,"agentTimestampMs":3000}},"timestamp":1783584024}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"e526ba42","stop_reason":"rate_limit","usage":{"inputTokens":86174,"outputTokens":1652,"totalTokens":87826,"cachedReadTokens":56960,"reasoningTokens":574,"modelCalls":5,"numTurns":5}}},"timestamp":1783584025}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"two"},"_meta":{"promptIndex":1}},"_meta":{"turnStartMs":6000,"totalTokens":22000}},"timestamp":1783584030}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second"}},"_meta":{"totalTokens":31628,"agentTimestampMs":9000}},"timestamp":1783584035}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"56468252","stop_reason":"end_turn","usage":{"inputTokens":198457,"outputTokens":6224,"totalTokens":204681,"cachedReadTokens":167680,"reasoningTokens":931,"modelCalls":7,"numTurns":7}}},"timestamp":1783584036}"#,
        "\n",
    );

    #[test]
    fn turn_completed_usage_fills_the_token_breakdown() {
        // The bug this covers: Grok's real token split rides `turn_completed`'s
        // `usage`, and the parser used to ignore it wholesale — synthesizing
        // usage from `_meta.totalTokens` with output and both cache buckets
        // hardcoded to 0, so the composer's breakdown read "output 0, cache 0"
        // for every Grok session no matter how much it had actually spent.
        let (_tmp, sessions) = fixture(SUMMARY, USAGE_UPDATES);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistants: Vec<_> = detail
            .turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::Assistant))
            .collect();
        assert_eq!(assistants.len(), 2);

        // Each prompt's own spend, split into DISJOINT buckets: the cached read
        // is carved OUT of `inputTokens` (86174 − 56960), not added to it.
        let first = assistants[0].usage.as_ref().expect("first usage");
        assert_eq!(first.input_tokens, 29_214);
        assert_eq!(first.output_tokens, 1_652);
        assert_eq!(first.cache_read_input_tokens, 56_960);
        assert_eq!(first.cache_creation_input_tokens, 0);
        // The four buckets re-sum to Grok's own `totalTokens` for that prompt,
        // which is what keeps the composer's "total" row honest.
        assert_eq!(total_of(first), 87_826);

        // The second prompt is taken at face value, NOT deltaed against the
        // first: `usage` is per-prompt, so the climb from 86174 to 198457 is the
        // second prompt being bigger, not a running total being restated.
        let second = assistants[1].usage.as_ref().expect("second usage");
        assert_eq!(second.input_tokens, 30_777);
        assert_eq!(second.output_tokens, 6_224);
        assert_eq!(second.cache_read_input_tokens, 167_680);
        assert_eq!(total_of(second), 204_681);

        // So the session total is the SUM of the two prompts. Deltaing would
        // report 204681 here and silently drop the first prompt's 87826.
        let stats = detail.session_stats.expect("session stats");
        let total = stats.total_usage.as_ref().expect("total usage");
        assert_eq!(total_of(total), 87_826 + 204_681);
        assert_eq!(stats.total_tokens, Some(292_507));
        assert_eq!(total.output_tokens, 1_652 + 6_224);
        assert_eq!(total.cache_read_input_tokens, 56_960 + 167_680);

        // And the context ring still reads the OCCUPANCY (`_meta.totalTokens`,
        // last value 31628) — not the 204681 the session spent getting there.
        assert_eq!(stats.context_window_used_tokens, Some(31_628));
        assert_eq!(stats.context_window_max_tokens, Some(500_000));
    }

    /// The sum the composer's "total" row shows for a usage record.
    fn total_of(usage: &TurnUsage) -> u64 {
        usage.input_tokens
            + usage.output_tokens
            + usage.cache_creation_input_tokens
            + usage.cache_read_input_tokens
    }

    #[test]
    fn repeated_prompt_usage_is_not_mistaken_for_a_running_total() {
        // Two prompts that spend exactly the same amount state the same numbers
        // — they are per-prompt, not a counter. A delta-based reading would see
        // "no movement" and report the second prompt as free.
        let usage = serde_json::json!({
            "inputTokens": 15_000, "outputTokens": 500,
            "totalTokens": 15_500, "cachedReadTokens": 6_000,
        });
        let first = prompt_usage(&usage).expect("first");
        let second = prompt_usage(&usage).expect("second");
        assert_eq!(total_of(&first), 15_500);
        assert_eq!(total_of(&second), 15_500);

        // An absent or all-zero `usage` still states nothing.
        assert!(prompt_usage(&serde_json::json!({})).is_none());
        assert!(prompt_usage(&serde_json::json!({
            "inputTokens": 0, "outputTokens": 0, "cachedReadTokens": 0,
        }))
        .is_none());
    }

    #[test]
    fn headless_only_usage_spellings_are_not_aliased() {
        // Grok's headless projector publishes `cacheReadInputTokens` with the
        // cache ALREADY subtracted from its `inputTokens`. Reading those names
        // here would double-subtract, so they are deliberately not accepted —
        // this record states nothing rather than something wrong.
        assert!(prompt_usage(&serde_json::json!({
            "cacheReadInputTokens": 41_000, "cache_creation_input_tokens": 2_000,
        }))
        .is_none());
    }

    #[test]
    fn cache_creation_tokens_are_carved_out_of_input_too() {
        // Newer Grok builds add `cacheCreationTokens` alongside the reads. It is
        // a slice of `inputTokens` on the same footing, so it is subtracted out
        // as well — leaving the four buckets summing to `totalTokens`.
        let usage = serde_json::json!({
            "inputTokens": 10_000, "outputTokens": 400, "totalTokens": 10_400,
            "cachedReadTokens": 6_000, "cacheCreationTokens": 1_500,
        });
        let parsed = prompt_usage(&usage).expect("usage");
        assert_eq!(parsed.input_tokens, 2_500);
        assert_eq!(parsed.cache_read_input_tokens, 6_000);
        assert_eq!(parsed.cache_creation_input_tokens, 1_500);
        assert_eq!(parsed.output_tokens, 400);
        assert_eq!(total_of(&parsed), 10_400);

        // Cache counters that overshoot `inputTokens` saturate to a zero
        // remainder rather than wrapping to a colossal input.
        let overshoot = serde_json::json!({
            "inputTokens": 100, "outputTokens": 10, "cachedReadTokens": 900,
        });
        let parsed = prompt_usage(&overshoot).expect("usage");
        assert_eq!(parsed.input_tokens, 0);
        assert_eq!(parsed.cache_read_input_tokens, 900);
    }

    /// Grok's own catalog outranks the id-shaped guess. The fixture's session
    /// model (summary `current_model_id` = `grok-4.5`) would infer 500K from its
    /// name, so a distinct cached window proves the ring read `models_cache.json`
    /// and not the heuristic.
    #[test]
    fn context_window_prefers_groks_models_cache() {
        let (tmp, sessions) = fixture(SUMMARY, RING_UPDATES);
        write(
            tmp.path(),
            "models_cache.json",
            r#"{"models":{"grok-4.5":{"info":{"context_window":314000}}}}"#,
        );
        let parser = GrokParser::with_base_dir(sessions);
        let stats = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap()
            .session_stats
            .expect("session stats");
        assert_eq!(stats.context_window_max_tokens, Some(314_000));
    }

    /// A BYO endpoint (`[model.<id>]` in `config.toml`) never appears in the
    /// fetched catalog, so its declared window is the second source.
    #[test]
    fn context_window_falls_back_to_byo_config_toml() {
        let (tmp, sessions) = fixture(SUMMARY, RING_UPDATES);
        write(
            tmp.path(),
            "models_cache.json",
            r#"{"models":{"some-other-model":{"info":{"context_window":999}}}}"#,
        );
        write(
            tmp.path(),
            "config.toml",
            "[models]\ndefault = \"grok-4.5\"\n\n[model.\"grok-4.5\"]\nmodel = \"grok-4.5\"\ncontext_window = 123456\n",
        );
        let parser = GrokParser::with_base_dir(sessions);
        let stats = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap()
            .session_stats
            .expect("session stats");
        assert_eq!(stats.context_window_max_tokens, Some(123_456));
    }

    /// No catalog on disk (the common case for a machine that only ever ran
    /// grok through codeg) → the name heuristic still supplies a window.
    #[test]
    fn context_window_falls_back_to_the_name_heuristic() {
        let empty_home = tempfile::tempdir().unwrap();
        let (_tmp, sessions) = fixture(SUMMARY, RING_UPDATES);
        let parser = GrokParser::with_base_dir(sessions);
        let stats = with_temp_grok_home(empty_home.path(), || {
            parser
                .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
                .unwrap()
                .session_stats
                .expect("session stats")
        });
        assert_eq!(stats.context_window_max_tokens, Some(500_000));
    }

    #[test]
    fn catalog_context_window_readers_reject_junk() {
        // Real `models_cache.json` shape (trimmed) → the model's own window.
        let cache = r#"{"grok_version":"1.0.0","models":{"grok-4.5":{"info":{
            "id":"grok-4.5","context_window":500000,"agent_type":"grok-build-plan"},
            "api_key":null}}}"#;
        assert_eq!(
            grok_context_window_from_models_cache(cache, "grok-4.5"),
            Some(500_000)
        );
        // Unknown model / malformed JSON / non-positive window → no opinion.
        assert_eq!(grok_context_window_from_models_cache(cache, "nope"), None);
        assert_eq!(
            grok_context_window_from_models_cache("{oops", "grok-4.5"),
            None
        );
        assert_eq!(
            grok_context_window_from_models_cache(
                r#"{"models":{"m":{"info":{"context_window":0}}}}"#,
                "m"
            ),
            None
        );
        // Same for the BYO TOML block.
        let toml = "[model.mine]\nmodel = \"mine\"\ncontext_window = 64000\n";
        assert_eq!(
            grok_context_window_from_config_toml(toml, "mine"),
            Some(64_000)
        );
        assert_eq!(grok_context_window_from_config_toml(toml, "other"), None);
        assert_eq!(grok_context_window_from_config_toml("[model", "mine"), None);
        assert_eq!(
            grok_context_window_from_config_toml("[model.mine]\ncontext_window = -1\n", "mine"),
            None
        );
    }

    #[test]
    fn assistant_turn_model_falls_back_to_summary() {
        // No in-stream modelId anywhere → the assistant turn's model is filled
        // from summary.json `current_model_id`, and without `params._meta` no
        // token stats are fabricated. The elapsed time is not a fabrication
        // though — the records are timestamped, so `backfill_turn_durations`
        // reads the reply's span straight off the prompt→reply clock.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail.turns.last().expect("assistant turn");
        assert_eq!(assistant.model.as_deref(), Some("grok-4.5"));
        assert!(assistant.usage.is_none());
        assert_eq!(
            assistant.duration_ms,
            Some(5_000),
            "1783584024 - 1783584019"
        );
    }

    #[test]
    fn assistant_turn_reasoning_effort_falls_back_to_summary() {
        // Grok only persists effort on summary.json (`reasoning_effort`), not
        // per-turn in updates.jsonl — stamp it onto assistant turns on reload.
        let summary = r#"{
            "info": {"id": "019f45e3-e1ef-7690-a29f-fe2554382b49", "cwd": "/Users/me/proj"},
            "session_summary": "Fallback summary",
            "generated_title": "Build the project",
            "created_at": "2026-07-09T07:59:50.598122Z",
            "updated_at": "2026-07-09T08:02:09.789572Z",
            "num_messages": 2,
            "current_model_id": "grok-4.5",
            "reasoning_effort": "high",
            "head_branch": "main"
        }"#;
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}},"timestamp":1783584024}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(summary, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail.turns.last().expect("assistant turn");
        assert_eq!(assistant.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn unwraps_use_tool_mcp_delegate_envelope() {
        // Grok wraps MCP calls in a `use_tool` envelope; history must peel it so
        // the delegation card classifies + shows the task, and the ack (carrying
        // task_id, in an MCP `rawOutput`) surfaces as the tool result.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"委派构建"}}},"timestamp":1783584019}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-d","title":"use_tool","rawInput":{"tool_name":"codeg-mcp__delegate_to_agent","tool_input":{"agent_type":"codex","working_dir":"/w","task":"run build"}},"_meta":{"x.ai/tool":{"name":"use_tool"}}}},"timestamp":1783584029}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-d","status":"completed","rawOutput":{"type":"MCP","tool_name":"delegate_to_agent","server_name":"codeg-mcp","output":{"OkayOutput":"Delegation successful. task_id=2dc85849-5426-44f7."}}}},"timestamp":1783584122}"#,
            "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584129}"#,
            "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail.turns.last().expect("assistant turn");

        let (tool_name, input_preview) = assistant
            .blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolUse {
                    tool_name,
                    input_preview,
                    ..
                } => Some((tool_name.clone(), input_preview.clone())),
                _ => None,
            })
            .expect("tool use present");
        // Tool name unwrapped to the MCP tool, not the "use_tool" wrapper.
        assert_eq!(tool_name, "codeg-mcp__delegate_to_agent");
        // Input preview is the inner tool_input (carries the task); wrapper gone.
        let input = input_preview.expect("input preview present");
        assert!(
            input.contains("\"task\":\"run build\""),
            "input carries the task: {input}"
        );
        assert!(
            !input.contains("tool_input"),
            "the wrapper is peeled: {input}"
        );

        // The MCP ack (with task_id) is the tool result.
        let result = assistant
            .blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolResult { output_preview, .. } => output_preview.clone(),
                _ => None,
            })
            .expect("tool result present");
        assert!(
            result.contains("task_id=2dc85849"),
            "the delegate ack surfaces as the result: {result}"
        );
    }

    #[test]
    fn use_tool_long_task_input_preview_stays_valid_json() {
        // A task prompt longer than the input cap must still yield VALID JSON
        // (string values truncated, structure intact) so the frontend delegation
        // card can JSON.parse it and recover the description — a raw byte
        // truncation of the whole serialized blob would corrupt it.
        let long_task = "x".repeat(GROK_TOOL_INPUT_CAP + 5_000);
        let updates = format!(
            concat!(
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"go"}}}}}},"timestamp":1783584019}}"#,
                "\n",
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"tool_call","toolCallId":"call-d","title":"use_tool","rawInput":{{"tool_name":"codeg-mcp__delegate_to_agent","tool_input":{{"agent_type":"codex","task":"{}"}}}}}}}},"timestamp":1783584029}}"#,
                "\n",
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}}},"timestamp":1783584129}}"#,
                "\n",
            ),
            long_task
        );
        let (_tmp, sessions) = fixture(SUMMARY, &updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let input = detail
            .turns
            .last()
            .unwrap()
            .blocks
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolUse { input_preview, .. } => input_preview.clone(),
                _ => None,
            })
            .expect("tool use present");
        // The stored preview parses as valid JSON, preserving the structure, and
        // stays within the input cap (a raw byte truncation would corrupt it).
        let parsed: Value = serde_json::from_str(&input).expect("input_preview must be valid JSON");
        assert_eq!(
            parsed.get("agent_type").and_then(Value::as_str),
            Some("codex")
        );
        assert!(parsed
            .get("task")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()));
        assert!(
            input.len() <= GROK_TOOL_INPUT_CAP,
            "preview stays within the cap: {} bytes",
            input.len()
        );
    }

    #[test]
    fn grok_mcp_input_preview_is_valid_and_bounded_for_compound_input() {
        // Every bloat vector at once — multiple oversized strings, a long array
        // of oversized strings, and multibyte/escaped text — must still yield
        // VALID JSON, preserve `agent_type`, keep a non-empty (truncated) `task`,
        // and respect the total serialized-size cap.
        let big = "x".repeat(GROK_TOOL_INPUT_CAP * 3);
        let multibyte = "行".repeat(GROK_TOOL_INPUT_CAP);
        let newlines = "\n".repeat(GROK_TOOL_INPUT_CAP);
        let input = serde_json::json!({
            "agent_type": "codex",
            "task": big,
            "working_dir": big,
            "notes": multibyte,
            "escaped": newlines,
            "list": [big, big, big],
        });
        let preview = grok_mcp_input_preview(&input).expect("preview produced");
        let parsed: Value = serde_json::from_str(&preview).expect("valid JSON");
        assert_eq!(
            parsed.get("agent_type").and_then(Value::as_str),
            Some("codex")
        );
        assert!(parsed
            .get("task")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()));
        assert!(
            preview.len() <= GROK_TOOL_INPUT_CAP,
            "compound preview is bounded: {} bytes",
            preview.len()
        );
    }

    #[test]
    fn missing_conversation_errors() {
        let (_tmp, sessions) = fixture(SUMMARY, UPDATES);
        let parser = GrokParser::with_base_dir(sessions);
        assert!(matches!(
            parser.get_conversation("does-not-exist"),
            Err(ParseError::ConversationNotFound(_))
        ));
    }

    #[test]
    fn honors_grok_home_env() {
        let home = resolve_grok_home_from(Some("/custom/grok".into()), Some("/home/me".into()));
        assert_eq!(home, PathBuf::from("/custom/grok"));
        let fallback = resolve_grok_home_from(None, Some("/home/me".into()));
        assert_eq!(fallback, PathBuf::from("/home/me/.grok"));
    }

    fn test_codeg_terminal_context() -> String {
        "<codeg_terminal_context version=\"1\">\n\
Selected shell: PowerShell 7\n\
Dialect: powershell\n\
Generate shell command lines using PowerShell syntax.\n\
ACP command+args requests may still execute directly.\n\
This context is authoritative for the current connection and supersedes\n\
earlier terminal context records.\n\
</codeg_terminal_context>"
            .to_string()
    }

    fn user_turn_texts(detail: &ConversationDetail) -> Vec<String> {
        detail
            .turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::User))
            .flat_map(|t| t.blocks.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hides_mandatory_route_chunk_and_keeps_one_grok_user_block() {
        let route = "Codeg mandatory delegation route: call delegate_to_agent exactly once";
        let prose = "Please implement the approved workflow plan";
        let summary = serde_json::json!({
            "info": {
                "id": "019f45e3-e1ef-7690-a29f-fe2554382b49",
                "cwd": "/Users/me/proj"
            },
            "session_summary": route,
            "generated_title": route,
            "created_at": "2026-07-09T07:59:50.598122Z",
            "updated_at": "2026-07-09T08:02:09.789572Z",
            "num_messages": 3,
            "current_model_id": "grok-4.5",
            "head_branch": "main"
        })
        .to_string();
        let updates = [
            serde_json::json!({
                "method": "session/update",
                "params": {"sessionId": "s", "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {"type": "text", "text": route},
                    "_meta": {"modelId": "grok-4.5", "promptIndex": 0}
                }},
                "timestamp": 1783584019_i64
            }),
            serde_json::json!({
                "method": "session/update",
                "params": {"sessionId": "s", "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {"type": "text", "text": prose},
                    "_meta": {"promptIndex": 0}
                }},
                "timestamp": 1783584020_i64
            }),
            serde_json::json!({
                "method": "session/update",
                "params": {"sessionId": "s", "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {"type": "text", "text": test_codeg_terminal_context()},
                    "_meta": {"promptIndex": 0}
                }},
                "timestamp": 1783584021_i64
            }),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        let (_tmp, sessions) = fixture(&summary, &updates);
        let detail = GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .expect("detail");

        assert_eq!(detail.summary.title.as_deref(), Some(prose));
        let user_turns = detail
            .turns
            .iter()
            .filter(|turn| matches!(turn.role, TurnRole::User))
            .collect::<Vec<_>>();
        assert_eq!(user_turns.len(), 1);
        assert_eq!(user_turns[0].blocks.len(), 1);
        assert_eq!(user_turn_texts(&detail), vec![prose.to_string()]);
    }

    #[test]
    fn hides_codeg_terminal_context_from_history_and_title() {
        let ctx = test_codeg_terminal_context();
        let real_plus = format!("real prompt\n\n{ctx}");
        let summary = format!(
            r#"{{
        "info": {{"id": "019f45e3-e1ef-7690-a29f-fe2554382b49", "cwd": "/Users/me/proj"}},
        "session_summary": {ctx_json},
        "generated_title": {ctx_json},
        "created_at": "2026-07-09T07:59:50.598122Z",
        "updated_at": "2026-07-09T08:02:09.789572Z",
        "num_messages": 3,
        "current_model_id": "grok-4.5",
        "head_branch": "main"
    }}"#,
            ctx_json = serde_json::to_string(&ctx).unwrap()
        );
        let updates = format!(
            r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":{ctx_json}}},"_meta":{{"promptIndex":0}}}}}},"timestamp":1783584019}}
{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":{real_json}}},"_meta":{{"promptIndex":1}}}}}},"timestamp":1783584020}}
{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"<codeg_terminal_context version=\"1\">partial"}},"_meta":{{"promptIndex":2}}}}}},"timestamp":1783584021}}
"#,
            ctx_json = serde_json::to_string(&ctx).unwrap(),
            real_json = serde_json::to_string(&real_plus).unwrap(),
        );

        let (_tmp, sessions) = fixture(&summary, &updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();

        assert_eq!(detail.summary.title.as_deref(), Some("real prompt"));
        let visible_user_texts = user_turn_texts(&detail);
        assert!(!visible_user_texts
            .iter()
            .any(|text| text.contains("Selected shell:")));
        assert!(visible_user_texts.iter().any(|text| text == "real prompt"));
        assert!(visible_user_texts
            .iter()
            .any(|text| text.contains("partial")));
    }

    // --- grok native ask_user_question answer recovery (chat_history.jsonl) ---

    #[test]
    fn history_answer_single_select() {
        let env = grok_history_answer_to_envelope(
            "User has answered your questions: \"你更喜欢哪种演示方式？\"=\"随便看看\". \
             You can now continue with the user's answers in mind.",
        )
        .unwrap();
        assert_eq!(env["declined"], false);
        assert_eq!(env["answers"][0]["header"], "");
        assert_eq!(env["answers"][0]["question"], "你更喜欢哪种演示方式？");
        assert_eq!(
            env["answers"][0]["selected"],
            serde_json::json!(["随便看看"])
        );
    }

    #[test]
    fn history_answer_multi_select_splits_on_comma() {
        // Grok joins a multi-select array with ", " inside the answer quotes.
        let env = grok_history_answer_to_envelope(
            "User has answered your questions: \"Which colors do you like?\"=\"Red, Green\". \
             You can now continue with the user's answers in mind.",
        )
        .unwrap();
        assert_eq!(
            env["answers"][0]["selected"],
            serde_json::json!(["Red", "Green"])
        );
    }

    #[test]
    fn history_answer_two_questions() {
        let env = grok_history_answer_to_envelope(
            "User has answered your questions: \"Q1\"=\"A1\", \"Q2\"=\"A2\". \
             You can now continue with the user's answers in mind.",
        )
        .unwrap();
        assert_eq!(env["answers"].as_array().unwrap().len(), 2);
        assert_eq!(env["answers"][0]["question"], "Q1");
        assert_eq!(env["answers"][0]["selected"], serde_json::json!(["A1"]));
        assert_eq!(env["answers"][1]["question"], "Q2");
        assert_eq!(env["answers"][1]["selected"], serde_json::json!(["A2"]));
    }

    #[test]
    fn history_answer_declined() {
        let env = grok_history_answer_to_envelope(
            "The user has indicated they have provided enough answers for the plan interview.\n\
             Stop asking clarifying questions and proceed to finish the plan.\n\n\
             Questions asked and answers provided:\n- \"Pick a size\"\n  (No answer provided)",
        )
        .unwrap();
        assert_eq!(env["declined"], true);
        assert_eq!(env["answers"], serde_json::json!([]));
    }

    #[test]
    fn history_answer_non_ask_is_none() {
        // A normal (non-ask) tool_result must never be mistaken for an answer.
        assert!(grok_history_answer_to_envelope("build ok\nexit code 0").is_none());
        assert!(grok_history_answer_to_envelope("").is_none());
        // Accepted prefix but no parseable pairs → None (leaves ToolResult as-is).
        assert!(
            grok_history_answer_to_envelope("User has answered your questions: none.").is_none()
        );
    }

    // Updates carrying grok's native ask_user_question (meta kind "ask_user"),
    // whose answer never lands in updates.jsonl — only in chat_history.jsonl.
    const ASK_UPDATES: &str = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"给我看看提问工具"},"_meta":{"promptIndex":0}}},"timestamp":1784334515}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-ask-0","title":"ask_user_question","rawInput":{"questions":[{"question":"你更喜欢哪种演示方式？","options":[{"label":"单选示例","description":"a"},{"label":"多选示例","description":"b"},{"label":"随便看看","description":"c"}]}]},"_meta":{"x.ai/tool":{"name":"ask_user_question","kind":"ask_user","namespace":"grok_build","label":"Ask User","read_only":true}}}},"timestamp":1784334520}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p0","stop_reason":"end_turn"}},"timestamp":1784334532}"#,
        "\n",
    );

    fn ask_session_dir(sessions: &Path) -> PathBuf {
        sessions
            .join("%2FUsers%2Fme%2Fproj")
            .join("019f45e3-e1ef-7690-a29f-fe2554382b49")
    }

    fn ask_detail(sessions: PathBuf) -> ConversationDetail {
        GrokParser::with_base_dir(sessions)
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap()
    }

    fn ask_result_output(detail: &ConversationDetail) -> Option<String> {
        detail
            .turns
            .iter()
            .flat_map(|t| t.blocks.iter())
            .find_map(|b| match b {
                ContentBlock::ToolResult { output_preview, .. } => Some(output_preview.clone()),
                _ => None,
            })
            .flatten()
    }

    #[test]
    fn injects_ask_answer_from_chat_history() {
        let (_tmp, sessions) = fixture(SUMMARY, ASK_UPDATES);
        write(
            &ask_session_dir(&sessions),
            "chat_history.jsonl",
            concat!(
                r#"{"type":"assistant","content":"演示","tool_calls":[{"id":"call-ask-0","name":"ask_user_question","arguments":"{}"}]}"#,
                "\n",
                r#"{"type":"tool_result","tool_call_id":"call-ask-0","content":"User has answered your questions: \"你更喜欢哪种演示方式？\"=\"随便看看\". You can now continue with the user's answers in mind."}"#,
                "\n",
            ),
        );
        let detail = ask_detail(sessions);
        let output = ask_result_output(&detail).expect("ask ToolResult output injected");
        let env: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(env["declined"], false);
        assert_eq!(env["answers"][0]["question"], "你更喜欢哪种演示方式？");
        assert_eq!(
            env["answers"][0]["selected"],
            serde_json::json!(["随便看看"])
        );
        assert_eq!(env["answers"][0]["header"], "");
    }

    #[test]
    fn injects_declined_ask_from_chat_history() {
        let (_tmp, sessions) = fixture(SUMMARY, ASK_UPDATES);
        write(
            &ask_session_dir(&sessions),
            "chat_history.jsonl",
            concat!(
                r#"{"type":"tool_result","tool_call_id":"call-ask-0","content":"The user has indicated they have provided enough answers for the plan interview.\n\nQuestions asked and answers provided:\n- \"你更喜欢哪种演示方式？\"\n  (No answer provided)"}"#,
                "\n",
            ),
        );
        let detail = ask_detail(sessions);
        let output = ask_result_output(&detail).expect("declined ask ToolResult output injected");
        let env: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(env["declined"], true);
        assert_eq!(env["answers"], serde_json::json!([]));
    }

    #[test]
    fn ask_without_chat_history_leaves_output_empty() {
        // No chat_history.jsonl → injection is a no-op; the ask ToolResult output
        // stays None (the pre-fix "未选择", never a crash).
        let (_tmp, sessions) = fixture(SUMMARY, ASK_UPDATES);
        let detail = ask_detail(sessions);
        assert!(ask_result_output(&detail).is_none());
    }
}
