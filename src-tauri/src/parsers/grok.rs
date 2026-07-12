use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::models::{
    AgentType, ContentBlock, ConversationDetail, ConversationSummary, MessageTurn, TurnRole,
    TurnUsage,
};
use crate::parsers::{
    compute_session_stats, folder_name_from_path, infer_context_window_max_tokens,
    latest_turn_total_usage_tokens, merge_context_window_stats, relocate_orphaned_tool_results,
    structurize_read_tool_output, title_from_user_text, truncate_str, AgentParser, ParseError,
};

/// Cap for a single tool result / tool input preview stored on a turn. Grok's
/// `tool_call_update.content` is **cumulative** (each update carries the whole
/// output so far), and long-running commands can emit tens of KB — bound it so
/// a single noisy command can't bloat a conversation detail payload.
const GROK_TOOL_OUTPUT_CAP: usize = 100_000;
const GROK_TOOL_INPUT_CAP: usize = 8_000;

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
///   background; `task_completed.task_snapshot` carries the authoritative final
///   `output` + `exit_code` (preferred over the streamed `tool_call_update`s).
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

    fn build_summary(&self, session_dir: &Path, session_id: &str) -> Option<ConversationSummary> {
        let parsed = parse_updates(&session_dir.join("updates.jsonl"));
        // A session that never produced any user/assistant/tool content (only
        // metadata) is treated as empty — matches the "metadata-only is not
        // listed" rule of the other parsers.
        if parsed.content_events == 0 {
            return None;
        }
        let meta = read_summary_json(session_dir);
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
        let title = meta
            .title
            .clone()
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
        let mut parsed = parse_updates(&session_dir.join("updates.jsonl"));
        let meta = read_summary_json(session_dir);

        // Defensive normalization shared with the other parsers: hoist any tool
        // result that landed outside its call's turn, and structurize file-read
        // output. Harmless no-ops when nothing matches.
        relocate_orphaned_tool_results(&mut parsed.turns);
        structurize_read_tool_output(&mut parsed.turns);

        // Fill assistant turns that carried no in-stream `modelId` with the
        // session model (summary `current_model_id`, else the first in-stream
        // model) so the message footer shows the model even for older/sparse
        // transcripts.
        if let Some(session_model) = meta.model.clone().or_else(|| parsed.model.clone()) {
            for turn in &mut parsed.turns {
                if matches!(turn.role, TurnRole::Assistant) && turn.model.is_none() {
                    turn.model = Some(session_model.clone());
                }
            }
        }

        // Grok sends no ACP `usage_update`, so the live meter stays empty; derive
        // the context ring here instead. Grok reports a cumulative per-turn token
        // count (mapped to `usage.input_tokens`), which is exactly the context
        // "used"; pair it with the model's window so the status bar shows the ring
        // (mirrors gemini/kimi/opencode — the bare `compute_session_stats` leaves
        // the context fields `None`).
        let session_model = meta.model.as_deref().or(parsed.model.as_deref());
        let session_stats = merge_context_window_stats(
            compute_session_stats(&parsed.turns),
            latest_turn_total_usage_tokens(&parsed.turns),
            infer_context_window_max_tokens(session_model),
        );
        let summary = self.summary_from(session_id, &meta, &parsed);

        ConversationDetail {
            summary,
            turns: parsed.turns,
            session_stats,
            transcript_watermark: None,
        }
    }

    /// Locate the `<session-uuid>` directory matching `conversation_id` across
    /// the `base_dir/<group>/` buckets (two shallow levels).
    fn find_session_dir(&self, conversation_id: &str) -> Option<PathBuf> {
        for group in read_subdirs(&self.base_dir) {
            let candidate = group.join(conversation_id);
            if candidate.join("updates.jsonl").is_file() {
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

// ---------------------------------------------------------------------------
// summary.json
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SummaryMeta {
    cwd: Option<String>,
    title: Option<String>,
    model: Option<String>,
    git_branch: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
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
}

fn parse_updates(path: &Path) -> ParsedUpdates {
    let Ok(file) = fs::File::open(path) else {
        return ParsedUpdates::default();
    };

    let mut out = ParsedUpdates::default();
    // The in-flight assistant turn, plus a `toolCallId → index-of-its-ToolResult`
    // map scoped to that turn (cleared on every turn boundary).
    let mut assistant: Option<MessageTurn> = None;
    let mut tool_result_idx: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // toolCallIds whose result `task_completed` already finalized. A backgrounded
    // command can emit a trailing (stale/cumulative) `tool_call_update` *after*
    // its `task_completed` — those must not clobber the authoritative snapshot
    // output. toolCallIds are unique within a session, so this is never cleared.
    let mut finalized_tools: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Stats for the in-flight turn (tokens/timing/model), applied to the
    // assistant turn when it is finalized. Reset at each turn boundary.
    let mut turn_meta = GrokTurnMeta::default();

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
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

        // Grok's per-turn stats live in the OUTER `params._meta` (token total +
        // timing) plus `update._meta.modelId`. Accumulate them into `turn_meta`
        // and apply at the turn boundary. A `user_message_chunk` opens a new
        // turn, so close+reset the prior turn's accumulator before observing it.
        let params_meta = v.pointer("/params/_meta");
        let update_meta = update.get("_meta");
        if kind == "user_message_chunk" {
            if let Some(prev) = assistant.as_mut() {
                turn_meta.apply(prev);
            }
            flush_assistant(&mut assistant, &mut out.turns, &mut tool_result_idx);
            turn_meta = GrokTurnMeta::default();
        }
        turn_meta.observe(params_meta, update_meta);

        match kind {
            "user_message_chunk" => {
                let text = update_text(update);
                out.content_events += 1;
                if out.first_user_text.is_none() && !text.trim().is_empty() {
                    out.first_user_text = Some(text.clone());
                }
                if out.model.is_none() {
                    out.model = update
                        .pointer("/_meta/modelId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                out.turns.push(MessageTurn {
                    id: String::new(), // assigned in a final pass
                    role: TurnRole::User,
                    blocks: vec![ContentBlock::Text { text }],
                    timestamp: now,
                    usage: None,
                    duration_ms: None,
                    model: None,
                    completed_at: None,
                });
            }
            "agent_message_chunk" => {
                out.content_events += 1;
                let text = update_text(update);
                append_text(ensure_assistant(&mut assistant, now), text);
            }
            "agent_thought_chunk" => {
                out.content_events += 1;
                let text = update_text(update);
                append_thinking(ensure_assistant(&mut assistant, now), text);
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
                let turn = ensure_assistant(&mut assistant, now);
                turn.blocks.push(ContentBlock::ToolUse {
                    tool_use_id: Some(id.clone()),
                    tool_name,
                    input_preview,
                    meta: None,
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
                // A trailing update after task_completed must not overwrite the
                // authoritative snapshot output.
                if !finalized_tools.contains(&id) {
                    let output = update_tool_output(update);
                    let failed = update.get("status").and_then(Value::as_str) == Some("failed");
                    apply_tool_result(assistant.as_mut(), &tool_result_idx, &id, output, failed);
                }
            }
            "task_completed" => {
                let snap = update.get("task_snapshot");
                let id = snap.map(|s| str_field(s, "task_id")).unwrap_or_default();
                let output = snap
                    .and_then(|s| s.get("output"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| truncate_str(s, GROK_TOOL_OUTPUT_CAP));
                let failed = snap
                    .and_then(|s| s.get("exit_code"))
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0);
                // task_completed is authoritative for a backgrounded command;
                // finalize the id so a trailing tool_call_update can't clobber it.
                apply_tool_result(assistant.as_mut(), &tool_result_idx, &id, output, failed);
                if !id.is_empty() {
                    finalized_tools.insert(id);
                }
            }
            "turn_completed" => {
                if let Some(mut turn) = assistant.take() {
                    turn_meta.apply(&mut turn);
                    turn.completed_at = Some(now);
                    out.turns.push(turn);
                }
                turn_meta = GrokTurnMeta::default();
                tool_result_idx.clear();
            }
            // task_backgrounded / plan / other extension updates carry no
            // distinct rendered content beyond what the tool stream already has.
            _ => {}
        }
    }

    // A session that ends mid-turn (no trailing `turn_completed`) still gets its
    // accumulated stats.
    if let Some(prev) = assistant.as_mut() {
        turn_meta.apply(prev);
    }
    flush_assistant(&mut assistant, &mut out.turns, &mut tool_result_idx);

    // Assign stable, unique, index-based ids (the transcript is append-only, so
    // positional ids are stable across re-parses).
    for (i, turn) in out.turns.iter_mut().enumerate() {
        turn.id = format!("grok-turn-{i}");
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

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
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
/// `rawOutput.output_for_prompt` (Bash/terminal), then an MCP `rawOutput`'s
/// `output` text (`use_tool`). All are cumulative, so the last update per call
/// carries the full output.
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
    let mut per_string = GROK_TOOL_INPUT_CAP;
    loop {
        let serialized = serde_json::to_string(&cap_json_string_values(input, per_string)).ok()?;
        if serialized.len() <= GROK_TOOL_INPUT_CAP || per_string == 0 {
            return Some(serialized);
        }
        per_string /= 2;
    }
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
/// the error flag (never clears it). Ordering vs. `task_completed` is enforced
/// by the caller's `finalized_tools` gate, not here.
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
/// in two sibling places the update loop otherwise ignores: token count + timing
/// in the OUTER `params._meta` (`totalTokens`, `turnStartMs`, `agentTimestampMs`)
/// and the model in `params.update._meta.modelId`. Grok reports a single
/// cumulative `totalTokens` (context/prompt tokens) rather than an input/output
/// split, so it maps to `input_tokens` — consistent with how other agents report
/// history-inclusive input. Duration is `end - start` in ms.
#[derive(Default)]
struct GrokTurnMeta {
    total_tokens: Option<u64>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    model: Option<String>,
}

impl GrokTurnMeta {
    /// Fold one update's metadata in. `params_meta` is `params._meta` (token
    /// total + timing); `update_meta` is `params.update._meta` (carries
    /// `modelId`). `totalTokens` is cumulative, so keep the max; `turnStartMs`
    /// is constant per turn (keep the min defensively); `agentTimestampMs`
    /// advances (keep the max as the turn end).
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

    /// Apply the accumulated stats to a finalized assistant turn. Never
    /// overwrites a field already set.
    fn apply(&self, turn: &mut MessageTurn) {
        if turn.model.is_none() {
            if let Some(model) = &self.model {
                turn.model = Some(model.clone());
            }
        }
        if turn.usage.is_none() {
            if let Some(tt) = self.total_tokens.filter(|t| *t > 0) {
                turn.usage = Some(TurnUsage {
                    input_tokens: tt,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                });
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

fn ensure_assistant(
    assistant: &mut Option<MessageTurn>,
    ts: DateTime<Utc>,
) -> &mut MessageTurn {
    if assistant.is_none() {
        *assistant = Some(MessageTurn {
            id: String::new(),
            role: TurnRole::Assistant,
            blocks: Vec::new(),
            timestamp: ts,
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
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
    use std::io::Write;

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
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"你会做什么"},"_meta":{"modelId":"grok-4.5","promptIndex":0}}},"timestamp":1783584019}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Thinking about it"}}},"timestamp":1783584019}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"我是 Grok"}}},"timestamp":1783584024}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p0","stop_reason":"end_turn"}},"timestamp":1783584024}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"执行 pnpm build"},"_meta":{"promptIndex":1}}},"timestamp":1783584029}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"正在执行"}}},"timestamp":1783584029}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-1","title":"run_terminal_command","rawInput":{"command":"pnpm build"},"_meta":{"x.ai/tool":{"name":"run_terminal_command","kind":"execute"}}}},"timestamp":1783584029}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"in_progress","content":[{"type":"content","content":{"type":"text","text":"partial output"}}]}},"timestamp":1783584033}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_completed","task_snapshot":{"task_id":"call-1","output":"build ok","exit_code":0}}},"timestamp":1783584122}"#, "\n",
        // Trailing (stale) update AFTER task_completed — must NOT clobber "build ok".
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"in_progress","content":[{"type":"content","content":{"type":"text","text":"STALE trailing output"}}]}},"timestamp":1783584123}"#, "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn"}},"timestamp":1783584129}"#, "\n",
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
        assert!(matches!(&turns[1].blocks[0], ContentBlock::Thinking { text } if text == "Thinking about it"));
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
        // task_completed output ("build ok") is authoritative over the streamed
        // "partial output", and exit_code 0 → not an error.
        let tool_result = last
            .blocks
            .iter()
            .find(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .unwrap();
        assert!(matches!(
            tool_result,
            ContentBlock::ToolResult { output_preview, is_error, .. }
                if output_preview.as_deref() == Some("build ok") && !*is_error
        ));
    }

    #[test]
    fn assistant_turn_carries_model_tokens_and_duration() {
        // Grok reports the footer's stats in two sibling metadata places the
        // loop must fold in: model in `update._meta.modelId`, and token total +
        // timing in the OUTER `params._meta` (`totalTokens` cumulative,
        // `turnStartMs` → `agentTimestampMs`).
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"modelId":"grok-4.5-fast","promptIndex":0}},"_meta":{"turnStartMs":1000,"totalTokens":100}},"timestamp":1783584019}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}},"_meta":{"totalTokens":500,"agentTimestampMs":3000}},"timestamp":1783584024}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"},"_meta":{"agentTimestampMs":5000}},"timestamp":1783584024}"#, "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail.turns.last().expect("assistant turn");
        assert!(matches!(assistant.role, TurnRole::Assistant));
        // In-stream modelId wins over the summary's current_model_id.
        assert_eq!(assistant.model.as_deref(), Some("grok-4.5-fast"));
        // Single cumulative totalTokens (max = 500) maps to input_tokens.
        let usage = assistant.usage.as_ref().expect("usage");
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 0);
        // Duration = last agentTimestampMs (5000) − turnStartMs (1000).
        assert_eq!(assistant.duration_ms, Some(4000));

        // Session stats aggregate the turn usage/duration.
        let stats = detail.session_stats.expect("session stats");
        assert_eq!(stats.total_usage.as_ref().unwrap().input_tokens, 500);
        assert_eq!(stats.total_duration_ms, 4000);
        // Context ring: cumulative tokens (500) as "used", paired with the
        // session model's window (summary current_model_id = grok-4.5 → 500K).
        // Without this the status bar shows no context ring for Grok.
        assert_eq!(stats.context_window_used_tokens, Some(500));
        assert_eq!(stats.context_window_max_tokens, Some(500_000));
        let pct = stats
            .context_window_usage_percent
            .expect("context window percent");
        assert!((pct - 0.1).abs() < 1e-6, "pct = {pct}");
    }

    #[test]
    fn assistant_turn_model_falls_back_to_summary() {
        // No in-stream modelId anywhere → the assistant turn's model is filled
        // from summary.json `current_model_id`, and without `params._meta` no
        // token/duration stats are fabricated.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1783584019}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}},"timestamp":1783584024}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584024}"#, "\n",
        );
        let (_tmp, sessions) = fixture(SUMMARY, updates);
        let parser = GrokParser::with_base_dir(sessions);
        let detail = parser
            .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
            .unwrap();
        let assistant = detail.turns.last().expect("assistant turn");
        assert_eq!(assistant.model.as_deref(), Some("grok-4.5"));
        assert!(assistant.usage.is_none());
        assert!(assistant.duration_ms.is_none());
    }

    #[test]
    fn unwraps_use_tool_mcp_delegate_envelope() {
        // Grok wraps MCP calls in a `use_tool` envelope; history must peel it so
        // the delegation card classifies + shows the task, and the ack (carrying
        // task_id, in an MCP `rawOutput`) surfaces as the tool result.
        let updates = concat!(
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"委派构建"}}},"timestamp":1783584019}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call-d","title":"use_tool","rawInput":{"tool_name":"codeg-mcp__delegate_to_agent","tool_input":{"agent_type":"codex","working_dir":"/w","task":"run build"}},"_meta":{"x.ai/tool":{"name":"use_tool"}}}},"timestamp":1783584029}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-d","status":"completed","rawOutput":{"type":"MCP","tool_name":"delegate_to_agent","server_name":"codeg-mcp","output":{"OkayOutput":"Delegation successful. task_id=2dc85849-5426-44f7."}}}},"timestamp":1783584122}"#, "\n",
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":1783584129}"#, "\n",
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
        assert!(!input.contains("tool_input"), "the wrapper is peeled: {input}");

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
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"go"}}}}}},"timestamp":1783584019}}"#, "\n",
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"tool_call","toolCallId":"call-d","title":"use_tool","rawInput":{{"tool_name":"codeg-mcp__delegate_to_agent","tool_input":{{"agent_type":"codex","task":"{}"}}}}}}}},"timestamp":1783584029}}"#, "\n",
                r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}}},"timestamp":1783584129}}"#, "\n",
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
        let parsed: Value =
            serde_json::from_str(&input).expect("input_preview must be valid JSON");
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
}
