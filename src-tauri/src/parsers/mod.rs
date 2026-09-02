pub mod acp_native;
pub mod antigravity;
pub mod claude;
pub mod cline;
pub mod codebuddy;
pub mod codex;
pub mod codex_code_mode;
pub mod cursor;
pub mod deepseek;
pub mod gemini;
pub mod grok;
pub mod hermes;
pub mod kimi_code;
pub mod opencode;
pub mod pi;
pub mod qoder;
mod summary_cache;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

/// A root of external agent-CLI transcript data, archived under
/// `external/<agent>/` by the optional "include conversation content" toggle.
/// These paths are owned by the respective CLIs — codeg only reads them.
#[derive(Clone)]
pub struct ExternalSource {
    /// Stable directory name inside the archive (`external/<agent>/`).
    pub agent: &'static str,
    /// Live source path (a directory, or a single file when `is_file`).
    pub root: PathBuf,
    pub is_file: bool,
    /// When `Some`, only entries whose first path component (relative to
    /// `root`) is in this allowlist are archived. Used to keep the backup to
    /// transcript/session data and exclude sibling credential/config/cache
    /// files in shared base dirs (e.g. `~/.gemini/oauth_creds.json`). `None`
    /// means the whole root is already transcript-scoped.
    pub include_top: Option<&'static [&'static str]>,
}

impl ExternalSource {
    /// The base directory a `external/<agent>/<rest>` entry restores under.
    /// For file sources that is the file's parent; for dir sources, the root.
    pub fn restore_base(&self) -> PathBuf {
        if self.is_file {
            self.root
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.root.clone())
        } else {
            self.root.clone()
        }
    }
}

/// Enumerate every external transcript source, resolved against the current
/// environment (honoring `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, etc.). Sources
/// whose root does not exist are still listed; callers skip missing roots.
pub fn external_transcript_sources() -> Vec<ExternalSource> {
    external_transcript_sources_for_runtime_env(&BTreeMap::new())
}

/// Enumerate external transcript sources using the effective environment of
/// the DeepSeek child process for its relocatable session store. Other parsers
/// retain their existing process-environment resolvers.
pub(crate) fn external_transcript_sources_for_runtime_env(
    deepseek_env: &BTreeMap<String, String>,
) -> Vec<ExternalSource> {
    let mut sources = vec![
        ExternalSource {
            agent: "claude",
            root: claude::resolve_claude_config_dir().join("projects"),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            agent: "codex",
            root: codex::resolve_codex_home_dir().join("sessions"),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            // Gemini's base dir mixes transcripts with credentials/config; only
            // pack the transcript/session subtrees, never `oauth_creds.json` etc.
            agent: "gemini",
            root: gemini::resolve_gemini_base_dir(),
            is_file: false,
            include_top: Some(&["tmp", "history", "projects.json"]),
        },
        ExternalSource {
            agent: "cline",
            root: cline::cline_data_dir(),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            agent: "opencode",
            root: opencode::resolve_opencode_base_dir().join("opencode.db"),
            is_file: true,
            include_top: None,
        },
        ExternalSource {
            // Hermes self-manages its session store at `~/.hermes/state.db`.
            // WAL caveat: `is_file` archives only the main DB file, not the
            // `-wal`/`-shm` sidecars, so a cold backup taken mid-write can miss
            // the newest un-checkpointed frames (same known limitation as
            // OpenCode). This does NOT affect live reads — the parser's `mode=ro`
            // connection sees committed WAL frames.
            agent: "hermes",
            root: hermes::resolve_hermes_home_dir().join("state.db"),
            is_file: true,
            include_top: None,
        },
        ExternalSource {
            // CodeBuddy stores its JSONL transcripts under
            // `~/.codebuddy/projects` — Claude Code's directory layout, but an
            // OpenAI Agents-SDK item record schema (see `parsers::codebuddy`).
            agent: "codebuddy",
            root: codebuddy::resolve_codebuddy_config_dir().join("projects"),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            // Kimi Code keeps a directory-per-session transcript store under
            // `~/.kimi-code/sessions/` plus a `session_index.jsonl` (the only
            // source of each session's working directory). Archive both, but
            // allowlist them so the sibling `config.toml` / `credentials/` /
            // `oauth/` are excluded (see `parsers::kimi_code`).
            agent: "kimi-code",
            root: kimi_code::resolve_kimi_code_home_dir(),
            is_file: false,
            include_top: Some(&["sessions", "session_index.jsonl"]),
        },
        ExternalSource {
            // Grok keeps a directory-per-session transcript store under
            // `~/.grok/sessions/<encoded-cwd>/<uuid>/` (relocatable via
            // `GROK_HOME`). `resolve_grok_home_dir()` points at `~/.grok`, so
            // scope the archive to the `sessions/` subtree — never the sibling
            // `auth.json` / `config.toml` / `bin/` under the same home.
            agent: "grok",
            root: grok::resolve_grok_home_dir().join("sessions"),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            // Cursor keeps a SQLite blob store per chat under
            // `~/.cursor/chats/<md5-of-cwd>/<uuid>/store.db`, and one per ACP
            // session under `~/.cursor/acp-sessions/<uuid>/store.db` (both
            // relocatable via `CURSOR_CONFIG_DIR`). Allowlist exactly those
            // two subtrees — never the sibling `cli-config.json` / `mcp.json`
            // / IDE state under the same home.
            agent: "cursor",
            root: cursor::resolve_cursor_config_dir(),
            is_file: false,
            include_top: Some(&["chats", "acp-sessions"]),
        },
        ExternalSource {
            // pi writes one JSONL per session under `~/.pi/agent/sessions/`
            // (relocatable via `PI_CODING_AGENT_SESSION_DIR` /
            // `PI_CODING_AGENT_DIR`). `resolve_pi_sessions_dir()` already points
            // at the `sessions/` subtree, so sibling credentials (`auth.json`,
            // `models.json`) under `~/.pi/agent` are never archived.
            agent: "pi",
            root: pi::resolve_pi_sessions_dir(),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            // Since deepseek-acp 0.6.0 a prompt can carry images, and the log
            // keeps only a `sha256:` reference — the pixels live in the
            // content-addressed store at `$DSH_HOME/attachments/v1/objects/`.
            // Without this source a backup restores every image as the
            // `[image …]` placeholder `parsers::deepseek` falls back to, which
            // is unrecoverable: the bytes exist nowhere else.
            //
            // A SEPARATE source rather than widening the one above, for two
            // reasons. `DEEPSEEK_ACP_SESSIONS_ROOT` relocates the logs
            // INDEPENDENTLY of `DSH_HOME`, so re-rooting both at `$DSH_HOME`
            // would silently stop archiving the logs of any deployment that
            // uses it. And `agent` is the restore key — `map_external_to_target`
            // resolves `external/<agent>/…` by `find(|s| s.agent == agent)`, so
            // two sources sharing a name would restore this one's entries under
            // the sessions root.
            //
            // Scoped to `objects/`: the siblings are `tmp/` (upload staging)
            // and `request-images/` (per-provider re-encodings derived from
            // `objects/`), neither of which is conversation content, and both
            // of which the agent rebuilds on demand.
            agent: "deepseek-attachments",
            root: deepseek::resolve_deepseek_attachments_root(),
            is_file: false,
            include_top: Some(&["objects"]),
        },
        ExternalSource {
            // Qoder keeps one JSONL per session under
            // `~/.qoder/projects/<encoded-cwd>/<sessionId>.jsonl` (relocatable
            // via `QODER_CONFIG_DIR`). The resolver already points at the
            // `projects/` subtree, so the sibling `settings.json` /
            // `security/` / `cache/` under `~/.qoder` are never archived.
            agent: "qoder",
            root: qoder::resolve_qoder_config_dir().join("projects"),
            is_file: false,
            include_top: None,
        },
        ExternalSource {
            // Antigravity keeps one SQLite trajectory + `.meta` sidecar per
            // session under `<GEMINI_HOME>/antigravity-acp/conversations`
            // (default `~/.gemini/...`). The root points at `conversations`
            // and NOT at `antigravity-acp` itself, whose siblings are the
            // OAuth token files (`acp_token.json`, `acp_business_token.json`)
            // — those must never end up in a backup archive.
            agent: "antigravity",
            root: antigravity::resolve_antigravity_sessions_dir(),
            is_file: false,
            include_top: None,
        },
    ];
    if let Some(root) = deepseek::resolve_deepseek_sessions_root_for_runtime_env(deepseek_env) {
        // DeepSeek Harness (deepseek-acp) keeps a directory-per-session log
        // store under `~/.dsh/sessions/<munged-cwd>/<uuid>/` (relocatable via
        // `DEEPSEEK_ACP_SESSIONS_ROOT` / `DSH_HOME`). The resolver already
        // points at the `sessions/` subtree, so the sibling `.credentials.yaml`
        // under `~/.dsh` is never archived. If the child's home was removed or
        // made relative, omit the source instead of scanning Codeg's profile.
        sources.push(ExternalSource {
            agent: "deepseek",
            root,
            is_file: false,
            include_top: None,
        });
    }
    sources
}

use regex::Regex;

use crate::models::{
    AgentType, ContentBlock, ConversationDetail, ConversationSummary, MessageTurn, SessionStats,
    TurnRole, TurnUsage,
};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Database error: {0}")]
    Db(#[from] sea_orm::DbErr),
    #[error("Conversation not found: {0}")]
    ConversationNotFound(String),
    #[error("Invalid data: {0}")]
    InvalidData(String),
}

pub struct RecoveryQuery<'a> {
    pub cwd: &'a str,
    pub approx: DateTime<Utc>,
    pub max_skew: chrono::Duration,
    pub ambiguity: chrono::Duration,
}

impl RecoveryQuery<'_> {
    pub fn cwd_matches(&self, summary: &ConversationSummary) -> bool {
        summary
            .folder_path
            .as_deref()
            .is_some_and(|path| path_eq_for_matching(path, self.cwd))
    }

    pub fn skew(&self, summary: &ConversationSummary) -> chrono::Duration {
        let started = (summary.started_at - self.approx).abs();
        match summary.ended_at {
            Some(ended) => started.min((ended - self.approx).abs()),
            None => started,
        }
    }
}

pub(crate) struct RecoveryCandidateTracker<T> {
    max_skew: chrono::Duration,
    ambiguity: chrono::Duration,
    best: Option<(chrono::Duration, T)>,
    second: Option<(chrono::Duration, T)>,
}

impl<T> RecoveryCandidateTracker<T> {
    pub(crate) fn new(query: &RecoveryQuery<'_>) -> Self {
        Self {
            max_skew: query.max_skew,
            ambiguity: query.ambiguity,
            best: None,
            second: None,
        }
    }

    pub(crate) fn consider(&mut self, skew: chrono::Duration, item: T) {
        match &self.best {
            None => self.best = Some((skew, item)),
            Some((best_skew, _)) if skew < *best_skew => {
                self.second = self.best.take();
                self.best = Some((skew, item));
            }
            Some(_) => match &self.second {
                None => self.second = Some((skew, item)),
                Some((second_skew, _)) if skew < *second_skew => {
                    self.second = Some((skew, item));
                }
                _ => {}
            },
        }
    }

    pub(crate) fn unique_winner(self) -> Option<T> {
        let (best_skew, best) = self.best?;
        if best_skew > self.max_skew {
            return None;
        }
        if let Some((second_skew, _)) = self.second {
            if second_skew <= self.max_skew && (second_skew - best_skew) < self.ambiguity {
                return None;
            }
        }
        Some(best)
    }
}

pub(crate) fn select_unique_recovery_match<'a>(
    summaries: impl IntoIterator<Item = &'a ConversationSummary>,
    query: &RecoveryQuery<'_>,
    accept: &dyn Fn(&ConversationSummary) -> bool,
) -> Option<&'a ConversationSummary> {
    let mut tracker = RecoveryCandidateTracker::new(query);
    for summary in summaries {
        if !accept(summary) || !query.cwd_matches(summary) {
            continue;
        }
        tracker.consider(query.skew(summary), summary);
    }
    tracker.unique_winner()
}

pub trait AgentParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError>;
    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError>;
    /// Parser-specific stale-session recovery. Default is fail-closed so a
    /// generic caller cannot fall back to listing every conversation.
    fn recover_conversation(
        &self,
        query: &RecoveryQuery<'_>,
        accept: &dyn Fn(&ConversationSummary) -> bool,
    ) -> Result<Option<ConversationDetail>, ParseError> {
        let _ = (query, accept);
        Ok(None)
    }
}

/// The ONE place a history parser is constructed.
///
/// Every caller goes through here so the internal `@agent` routing frame is
/// stripped from every agent's history — not just the one agent whose parser
/// happens to know about it. `codeg-mcp` is injected into every MCP-capable
/// agent, so the frame lands in each of their native transcripts; a per-parser
/// fix would silently miss whichever parser was written next.
pub fn build_agent_parser(agent_type: AgentType) -> Box<dyn AgentParser> {
    let inner: Box<dyn AgentParser> = match agent_type {
        AgentType::ClaudeCode => Box::new(claude::ClaudeParser::new()),
        AgentType::Codex => Box::new(codex::CodexParser::new()),
        AgentType::OpenCode => Box::new(opencode::OpenCodeParser::new()),
        AgentType::Gemini => Box::new(gemini::GeminiParser::new()),
        AgentType::Cline => Box::new(cline::ClineParser::new()),
        AgentType::Hermes => Box::new(hermes::HermesParser::new()),
        AgentType::CodeBuddy => Box::new(codebuddy::CodeBuddyParser::new()),
        AgentType::KimiCode => Box::new(kimi_code::KimiCodeParser::new()),
        AgentType::Pi => Box::new(pi::PiParser::new()),
        AgentType::Grok => Box::new(grok::GrokParser::new()),
        AgentType::Cursor => Box::new(cursor::CursorParser::new()),
        AgentType::DeepSeek => Box::new(deepseek::DeepSeekParser::new()),
        AgentType::Qoder => Box::new(qoder::QoderParser::new()),
        AgentType::Antigravity => Box::new(antigravity::AntigravityParser::new()),
        // Custom ACP agents have no native store to reverse-engineer; their
        // history is codeg's own ACP transcript.
        AgentType::Custom(_) => Box::new(acp_native::AcpNativeParser::new(agent_type)),
    };
    route_sanitized(inner)
}

pub(crate) fn build_agent_parser_with_runtime_env(
    agent_type: AgentType,
    runtime_env: &BTreeMap<String, String>,
) -> Option<Box<dyn AgentParser>> {
    match agent_type {
        AgentType::DeepSeek => deepseek::DeepSeekParser::from_runtime_env(runtime_env)
            .map(|parser| route_sanitized(Box::new(parser))),
        _ => Some(build_agent_parser(agent_type)),
    }
}

fn route_sanitized(inner: Box<dyn AgentParser>) -> Box<dyn AgentParser> {
    Box::new(RouteSanitized(inner))
}

/// Removes Codeg's internal `@agent` routing frame from whatever a parser read
/// back out of an agent's own transcript.
///
/// Only complete frames that re-render byte-for-byte are touched (see
/// [`crate::acp::agent_mentions::strip_internal_agent_routes`]), and the
/// separator that opens one is scrubbed from every prompt at ingress, so
/// look-alike user prose is never eligible. A BLOCK left with no content after
/// stripping carried nothing but the frame and is dropped; a user turn left
/// with no content at all was a transport-only record and is dropped rather
/// than rendered as a phantom turn.
///
/// Not every agent hands the frame back the way it was sent: Antigravity's ACP
/// server joins the prompt's text blocks with a space and replaces each
/// separator with one, so its trajectory holds the frame's body with the
/// separators gone. That form is matched on the body alone and removed here
/// too — which is also why the [`sanitize_text`] trim below is not optional for
/// those agents, since the spaces that replaced the separators survive the
/// strip.
///
/// Dropping the emptied block — not just emptying it — is what keeps an
/// `@`-mention turn from growing a blank band when it is reopened from history.
/// `append_agent_routes` appends the frame as its OWN prompt block, and the
/// parsers that keep one block per recorded text item (claude's
/// `extract_user_content`, `acp_native`'s `prompt_blocks`) therefore hand back
/// `[Text(prose), Text(frame)]`. Emptying the second in place left a zero-height
/// text part that still takes a `space-y-4` gap in the bubble.
///
/// The Codex parser additionally handles route-only records STRUCTURALLY
/// (canonical-channel coverage is positional and cannot be repaired after the
/// fact); this pass is idempotent on top of that.
struct RouteSanitized(Box<dyn AgentParser>);

impl AgentParser for RouteSanitized {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut summaries = self.0.list_conversations()?;
        for summary in &mut summaries {
            sanitize_summary(summary);
        }
        Ok(summaries)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        self.0
            .get_conversation(conversation_id)
            .map(sanitize_detail)
    }

    fn recover_conversation(
        &self,
        query: &RecoveryQuery<'_>,
        accept: &dyn Fn(&ConversationSummary) -> bool,
    ) -> Result<Option<ConversationDetail>, ParseError> {
        self.0
            .recover_conversation(query, accept)
            .map(|detail| detail.map(sanitize_detail))
    }
}

fn sanitize_detail(mut detail: ConversationDetail) -> ConversationDetail {
    let before = detail.turns.len();
    detail.turns.retain_mut(|turn| {
        if !matches!(turn.role, TurnRole::User) {
            return true;
        }
        turn.blocks.retain_mut(|block| {
            let ContentBlock::Text { text } = block else {
                return true;
            };
            // Only a block that actually HELD a frame is eligible to go: an
            // empty text block a parser produced for some other reason is
            // left exactly as it was found.
            !(sanitize_text(text) && text.trim().is_empty())
        });
        // A turn whose ONLY content was the frame carried no user message.
        !turn.blocks.iter().all(|block| match block {
            ContentBlock::Text { text } => text.trim().is_empty(),
            _ => false,
        })
    });
    // Keep the count the sidebar shows in step with the turns actually
    // rendered; the summary rides along inside the detail.
    let dropped = (before - detail.turns.len()) as u32;
    detail.summary.message_count = detail.summary.message_count.saturating_sub(dropped);
    sanitize_summary(&mut detail.summary);
    detail
}

fn sanitize_summary(summary: &mut ConversationSummary) {
    if let Some(title) = summary.title.as_mut() {
        sanitize_text(title);
        // Titles are capped by their parser BEFORE reaching here, so a frame can
        // straddle the cut and survive `sanitize_text`, which only removes whole
        // frames. Anything from a leftover marker on is truncated frame.
        crate::acp::agent_mentions::cut_at_route_frame_marker(title);
        if title.trim().is_empty() {
            summary.title = None;
        }
    }
}

/// Strip every complete frame from `text`, reporting whether one was there.
///
/// The trailing trim only runs when a frame WAS removed, and it is load-bearing
/// for the agents that join a prompt's text blocks into one record with a blank
/// line: `strip_internal_agent_routes` absorbs a single newline adjacent to the
/// frame, so `"prose\n\n<frame>"` comes back as `"prose\n"` — which
/// `whitespace-pre-wrap` paints as an extra blank line under the message. Text
/// with no frame in it is never touched.
fn sanitize_text(text: &mut String) -> bool {
    // Single scan short-circuit: history with no frame pays one memchr per
    // string, not a parse attempt.
    if !crate::acp::agent_mentions::contains_internal_agent_routes(text) {
        return false;
    }
    *text = crate::acp::agent_mentions::strip_internal_agent_routes(text);
    text.truncate(text.trim_end().len());
    true
}

/// Expand a leading `~` in a relocation env var, for the agents whose OWN
/// resolver does that.
///
/// Handles a bare `~` and a `~/` — or, on Windows, `~\` — prefix.
///
/// A `~user` form is left VERBATIM, which is a deliberate divergence rather
/// than a match: Python's `os.path.expanduser` does attempt a passwd lookup for
/// it on unix, and Node's does not. Resolving another account's home here would
/// mean duplicating that lookup to guess at a directory, so the value is passed
/// through instead — and because a literal `~user/...` is not an absolute path,
/// every consumer of this treats it as unresolvable and fails closed (the fs
/// sandbox refuses the slot; the Antigravity settings sync refuses the write).
/// The cost is that this rare form is unsupported, not that it resolves wrongly.
///
/// Deliberately NOT applied everywhere. Antigravity runs `os.path.expanduser`
/// on `GEMINI_HOME` (`acp_server/paths.py`) and DeepSeek expands `DSH_HOME`
/// (`dsh-home-paths`' `expandHomePath`), but Hermes's `get_hermes_home` is a
/// bare `Path(val.strip())` and Codex, Claude and the rest are likewise
/// verbatim. Expanding for one of those would point codeg at `$HOME/...` while
/// the agent used a literal `~` directory — and, in the fs sandbox, would hand
/// out `$HOME` as a writable root the user never selected.
///
/// Shared so the rule lives in ONE place: it is mirrored by
/// `acp::file_system_runtime`'s root table (`EXPANDS_TILDE`), and a copy that
/// drifts from the resolver it mirrors is exactly how the agent ends up unable
/// to write its own directory.
pub fn expand_home_prefix(value: &str, home_dir: Option<&PathBuf>) -> PathBuf {
    let Some(home) = home_dir else {
        return PathBuf::from(value);
    };
    if value == "~" {
        return home.clone();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

/// True when `id` is safe to embed as a single filename component beneath a
/// session's `subagents/` directory (Claude Code's and CodeBuddy's sub-agent
/// transcript layout). The id is read straight from transcript JSON
/// (`agentId` / `subAgent.sessionId`), so a corrupted or hostile transcript
/// could otherwise smuggle a path that escapes the directory once it is joined
/// and a file is opened.
///
/// Rejects: empty, a path separator (`/` or `\`), a parent ref (`..`), a colon
/// (Windows drive prefix `C:` / NTFS alternate-data-stream), or a NUL. The
/// checks are conservative and platform-independent — we reject `:` and `\`
/// even on Unix (where they are legal filename chars) so the same id can never
/// escape if the transcript is later read on Windows, where `Path::join("C:x")`
/// silently replaces the whole base path.
pub fn is_safe_subagent_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.contains(':')
        && !id.contains('\0')
}

/// Punctuation the serializer escapes with a leading backslash inside a
/// reference label (mirrors `escapeMarkdownText` in `src/lib/reference-text.ts`
/// and the class in the frontend `unescapeReferenceLabel`).
fn is_escapable_reference_punct(c: char) -> bool {
    matches!(
        c,
        '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '(' | ')' | '<' | '>'
    )
}

/// Reverse the serializer's label escaping: drop the backslash from each escaped
/// inline-significant punctuation char so the recovered label reads literally.
/// Mirrors the frontend `unescapeReferenceLabel`.
fn unescape_reference_label(label: &[char]) -> String {
    let mut out = String::with_capacity(label.len());
    let mut i = 0;
    while i < label.len() {
        if label[i] == '\\' && i + 1 < label.len() && is_escapable_reference_punct(label[i + 1]) {
            out.push(label[i + 1]);
            i += 2;
        } else {
            out.push(label[i]);
            i += 1;
        }
    }
    out
}

/// Mirror ECMAScript's `/\s/` — the whitespace class the frontend
/// `foldReferenceLinks` (`src/lib/reference-link.ts`) scans destinations with —
/// so this port stays in step with it. It deliberately differs from Rust's
/// `char::is_whitespace()` in exactly two code points: `U+FEFF` (BOM) is
/// whitespace to JS but not to Rust, and `U+0085` (NEL) is whitespace to Rust
/// but not to JS. The set is ECMAScript WhiteSpace + LineTerminator.
fn is_markdown_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'      // tab, LF, VT, FF, CR
            | '\u{0020}'             // space
            | '\u{00A0}'             // no-break space
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'             // line separator
            | '\u{2029}'             // paragraph separator
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'             // zero-width no-break space (BOM)
    )
}

/// Whether the backslash at `k` escapes the next character. CommonMark never
/// lets a backslash escape whitespace, so `\` + whitespace ENDS (not extends) a
/// label/destination scan — only `\` + a non-whitespace char is a real escape.
fn reference_escapes_next(chars: &[char], k: usize) -> bool {
    chars.get(k) == Some(&'\\')
        && chars
            .get(k + 1)
            .is_some_and(|c| !is_markdown_whitespace(*c))
}

/// If a well-formed `(destination)` begins at `start`, return the index just
/// past its closing `)`; otherwise `None`. Mirrors the frontend `destinationEnd`
/// and the serializer's two forms: a `<…>`-wrapped destination (interior `\`,
/// `<`, `>` backslash-escaped) or a bare run with no `(`, `)`, whitespace, `<` or
/// `>`.
fn reference_destination_end(chars: &[char], start: usize) -> Option<usize> {
    let n = chars.len();
    if start >= n || chars[start] != '(' {
        return None;
    }
    let mut k = start + 1;
    if chars.get(k) == Some(&'<') {
        k += 1;
        while k < n {
            if reference_escapes_next(chars, k) {
                k += 2;
                continue;
            }
            match chars[k] {
                '>' => {
                    return if chars.get(k + 1) == Some(&')') {
                        Some(k + 2)
                    } else {
                        None
                    };
                }
                // An unescaped `<` or a line break is forbidden inside `<…>`;
                // bailing here also bounds the scan so a missing `>` stops at the
                // next `<` instead of running to EOF (keeps adversarial input
                // linear).
                '<' | '\n' | '\r' => return None,
                _ => k += 1,
            }
        }
        return None;
    }
    while k < n {
        if reference_escapes_next(chars, k) {
            k += 2;
            continue;
        }
        let c = chars[k];
        if c == ')' {
            return Some(k + 1);
        }
        if c == '(' || c == '<' || c == '>' || is_markdown_whitespace(c) {
            return None;
        }
        k += 1;
    }
    None
}

/// Replace every inline `[label](destination)` reference link in `text` with its
/// unescaped `label`, leaving all other prose (including malformed `[…]`/`(…)`
/// fragments and invocation tokens like `@Codex`) untouched.
///
/// This is the Rust counterpart of the frontend canonical fold
/// (`foldReferenceLinks` in `src/lib/reference-link.ts`) and MUST stay in step
/// with it: a single O(n) left-to-right scan over a stack of unmatched `[`
/// positions, matching each `]` against the most recent opener so a balanced
/// nested label closes at the right bracket, requiring a non-empty label and a
/// well-formed `(dest)` for a link, and recovering later links after a
/// stray/unbalanced `[`. Used to derive conversation titles from a user's first
/// message: folding BEFORE truncation means a long `file://` destination can
/// never be sliced mid-link into an unterminable `[label](file://…` fragment.
pub fn fold_reference_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    // Start of the pending prose run; flushed before each link and at the end.
    let mut text_start = 0usize;
    // Indices of `[` seen but not yet matched by a `]` (most recent on top).
    let mut openers: Vec<usize> = Vec::new();
    let mut i = 0usize;

    while i < n {
        if reference_escapes_next(&chars, i) {
            // `\[` / `\]` (and any `\x`) is literal — skip both chars.
            i += 2;
            continue;
        }
        match chars[i] {
            '[' => {
                openers.push(i);
                i += 1;
            }
            ']' if !openers.is_empty() => {
                let open = openers.pop().expect("openers is non-empty");
                match reference_destination_end(&chars, i + 1) {
                    // A link needs a well-formed `(dest)` right after `]` and a
                    // non-empty label between the brackets.
                    Some(end) if i > open + 1 => {
                        out.extend(chars[text_start..open].iter());
                        out.push_str(&unescape_reference_label(&chars[open + 1..i]));
                        // Everything up to `open` is committed, so any still-open
                        // outer `[` can no longer span a link.
                        openers.clear();
                        i = end;
                        text_start = end;
                    }
                    // Not a link: keep the brackets in the pending prose run and
                    // keep scanning so a later valid link is still found.
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out.extend(chars[text_start..n].iter());
    out
}

/// Derive a conversation title from a user's first message: fold inline
/// reference links to their labels, then cap the length. Folding first ensures a
/// `[name](file://<long path>)` mention becomes `name` instead of a raw — and,
/// once truncated, unterminable — Markdown link.
pub fn title_from_user_text(text: &str) -> String {
    truncate_str(&fold_reference_links(text), 100)
}

/// Exact Codeg version-1 wire envelope for terminal shell context (see
/// `acp::terminal_context::render_terminal_prompt_context`). History parsers must
/// hide only complete envelopes — never partial XML or user-authored similar tags.
fn terminal_context_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r#"(?ms)<codeg_terminal_context version="1">\r?\n"#,
            r#"Selected shell: [^\r\n]+\r?\n"#,
            r#"(?:Dialect: cmd\r?\nGenerate shell command lines using CMD syntax\."#,
            r#"|Dialect: powershell\r?\nGenerate shell command lines using PowerShell syntax\."#,
            r#"|Dialect: posix\r?\nGenerate shell command lines using POSIX syntax\."#,
            r#"|Dialect: custom\r?\nGenerate shell command lines using the selected custom shell's syntax\.)\r?\n"#,
            r#"ACP command\+args requests may still execute directly\.\r?\n"#,
            r#"This context is authoritative for the current connection and supersedes\r?\n"#,
            r#"earlier terminal context records\.\r?\n"#,
            r#"</codeg_terminal_context>(?:\r?\n)?"#,
        ))
        .expect("valid terminal context regex")
    })
}

/// Strip complete Codeg version-1 terminal context envelopes from transcript text.
/// Replaces each match with a single newline so adjacent user prose stays separated.
pub fn strip_codeg_terminal_context(text: &str) -> Cow<'_, str> {
    terminal_context_regex().replace_all(text, "\n")
}

const MANDATORY_ROUTE_PREFIX: &str = "Codeg mandatory delegation route:";

fn is_pure_mandatory_route_text(text: &str) -> bool {
    let mut saw_non_empty = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        saw_non_empty = true;
        if !line.starts_with(MANDATORY_ROUTE_PREFIX) {
            return false;
        }
    }
    saw_non_empty
}

/// User-visible prompt text after removing complete terminal context envelopes
/// and pure mandatory-route blocks. Returns `None` when nothing visible remains.
pub fn visible_user_text(text: &str) -> Option<String> {
    let stripped = strip_codeg_terminal_context(text);
    if is_pure_mandatory_route_text(&stripped) {
        return None;
    }
    let visible = stripped.trim();
    (!visible.is_empty()).then(|| visible.to_string())
}

/// Sanitize a native AI/session title so a complete context envelope cannot
/// become a list title. Truncate only after this returns `Some`.
pub fn visible_title(title: Option<String>) -> Option<String> {
    title.and_then(|value| visible_user_text(&value))
}

/// Strip terminal context and pure mandatory routes from text blocks; drop empty text blocks.
/// Returns whether any visible content remains (images/tools count as visible).
pub fn sanitize_user_blocks(blocks: &mut Vec<ContentBlock>) -> bool {
    for block in blocks.iter_mut() {
        if let ContentBlock::Text { text } = block {
            *text = visible_user_text(text).unwrap_or_default();
        }
    }
    blocks.retain(|block| !matches!(block, ContentBlock::Text { text } if text.is_empty()));
    !blocks.is_empty()
}

/// Fill in `duration_ms` for assistant turns whose agent reports no timing of
/// its own, by tiling the conversation timeline.
pub fn backfill_turn_durations(turns: &mut [MessageTurn], turn_starts: &[DateTime<Utc>]) {
    let mut cursor: Option<DateTime<Utc>> = None;
    let mut next_start = 0usize;

    for turn in turns.iter_mut() {
        let end = turn.completed_at.unwrap_or(turn.timestamp);
        while next_start < turn_starts.len() && turn_starts[next_start] <= end {
            advance_duration_cursor(&mut cursor, turn_starts[next_start]);
            next_start += 1;
        }

        if matches!(turn.role, TurnRole::Assistant) && turn.duration_ms.is_none() {
            if let Some(start) = cursor {
                let ms = (end - start).num_milliseconds();
                if ms > 0 {
                    turn.duration_ms = Some(ms as u64);
                }
            }
        }

        advance_duration_cursor(&mut cursor, end);
    }
}

fn advance_duration_cursor(cursor: &mut Option<DateTime<Utc>>, candidate: DateTime<Utc>) {
    if cursor.is_none_or(|current| candidate > current) {
        *cursor = Some(candidate);
    }
}

/// Aggregate turn-level usage and duration into a single `SessionStats`.
///
/// The two halves stand on their own: an agent that times its turns but reports
/// no token split still gets its duration (mirrors `acp_native::session_stats`,
/// which already had to hand-roll this). Token totals stay `None` in that case
/// rather than becoming a row of zeros — the footer reads `None` as "this agent
/// doesn't say" and omits the breakdown, where zeros would read as "it says the
/// reply was free".
pub fn compute_session_stats(turns: &[MessageTurn]) -> Option<SessionStats> {
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut total_cache_create = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_duration = 0u64;
    let mut has_usage = false;

    for turn in turns {
        if let Some(ref u) = turn.usage {
            total_in += u.input_tokens;
            total_out += u.output_tokens;
            total_cache_create += u.cache_creation_input_tokens;
            total_cache_read += u.cache_read_input_tokens;
            has_usage = true;
        }
        if let Some(d) = turn.duration_ms {
            total_duration += d;
        }
    }

    if !has_usage && total_duration == 0 {
        return None;
    }

    Some(SessionStats {
        total_usage: has_usage.then_some(TurnUsage {
            input_tokens: total_in,
            output_tokens: total_out,
            cache_creation_input_tokens: total_cache_create,
            cache_read_input_tokens: total_cache_read,
        }),
        total_tokens: has_usage
            .then_some(total_in + total_out + total_cache_create + total_cache_read),
        total_duration_ms: total_duration,
        context_window_used_tokens: None,
        context_window_max_tokens: None,
        context_window_usage_percent: None,
    })
}

fn model_capacity_suffix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\[\s*([0-9]+(?:\.[0-9]+)?)\s*([km])\s*\]\s*$")
            .expect("valid model capacity regex")
    })
}

/// Matches the SDK's *id* spelling of Anthropic's 1M-context lane, where `1m`
/// is its own delimited token (`claude-opus-4-6-1m`). `\b1m\b` is the same
/// test claude-agent-acp's `inferContextWindowFromModel` applies, and it
/// deliberately does not match embedded runs like `10m`.
fn claude_one_million_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b1m\b").expect("valid claude 1m id regex"))
}

/// Whether `model` names Anthropic's 1M-context lane through the SDK's id
/// spelling (`claude-opus-4-6-1m`). The CLI's *display* spelling
/// (`claude-sonnet-5[1m]`) carries the same meaning but is handled by
/// [`parse_model_capacity_suffix`], which reads the bracketed number directly.
fn is_claude_one_million_context_id(model: &str) -> bool {
    claude_one_million_id_regex().is_match(model)
}

fn parse_model_capacity_suffix(model: &str) -> Option<u64> {
    let captures = model_capacity_suffix_regex().captures(model.trim())?;
    let value = captures.get(1)?.as_str().parse::<f64>().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    let unit = captures
        .get(2)
        .map(|m| m.as_str().to_ascii_lowercase())
        .unwrap_or_default();
    let multiplier = match unit.as_str() {
        "m" => 1_000_000.0,
        "k" => 1_000.0,
        _ => return None,
    };

    Some((value * multiplier) as u64)
}

pub fn infer_context_window_max_tokens(model: Option<&str>) -> Option<u64> {
    let raw = model?.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(suffixed_limit) = parse_model_capacity_suffix(raw) {
        return Some(suffixed_limit);
    }

    let normalized = raw
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .split(':')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();

    // Anthropic's default lane is 200K; the 1M lane is opt-in and shows up in
    // the model id itself. Agents other than Claude Code record the id the way
    // their backend named it, so the marker survives here — unlike Claude
    // Code's own transcripts, where it is stripped (see
    // `claude::claude_context_window_max_tokens_for_model`).
    if normalized.starts_with("claude") {
        if is_claude_one_million_context_id(&normalized) {
            return Some(1_000_000);
        }
        return Some(200_000);
    }
    if normalized.starts_with("gemini") {
        return Some(1_000_000);
    }
    if normalized.starts_with("kimi") {
        return Some(262_144);
    }
    if normalized.starts_with("grok") {
        // Context windows per x.ai docs (docs.x.ai/developers/models, 2026-08).
        // Grok's model names churn, so match the known families before the
        // generic fallback, most-specific first:
        //   grok-4.5 / grok-4.6   → 500K
        //   grok-4.3 / grok-4.20  → 1M
        //   grok-build-* / grok-code-fast-1 → 256K (coding models; the latter
        //                           is 256K despite the "fast" in its name)
        //   general -fast (grok-4-fast) → 2M
        // Default any unknown grok model to the conservative 256K rather than
        // guessing high.
        if normalized.contains("4.5") || normalized.contains("4.6") {
            return Some(500_000);
        }
        if normalized.contains("4.3") || normalized.contains("4.20") {
            return Some(1_000_000);
        }
        if normalized.contains("code") || normalized.contains("build") {
            return Some(256_000);
        }
        if normalized.contains("fast") {
            return Some(2_000_000);
        }
        return Some(256_000);
    }

    match normalized.as_str() {
        "gpt-5.2-codex" | "gpt-5.1-codex-max" | "gpt-5.1-codex-mini" | "gpt-5.2" => Some(258_000),
        "gpt-5.1" | "gpt-5.1-codex" | "gpt-4o" | "gpt-4o-mini" | "gpt-4-turbo" | "o1-mini"
        | "o1-preview" => Some(128_000),
        "gpt-4" => Some(8_192),
        "o3" | "o3-mini" | "o1" => Some(200_000),
        _ => {
            if normalized.starts_with("gpt-5") {
                Some(258_000)
            } else if normalized.starts_with("gpt-4o")
                || normalized.starts_with("gpt-4.1")
                || normalized.starts_with("gpt-4-turbo")
            {
                Some(128_000)
            } else if normalized.starts_with("o3") || normalized == "o1" {
                Some(200_000)
            } else if normalized.starts_with("o1-mini") || normalized.starts_with("o1-preview") {
                Some(128_000)
            } else {
                None
            }
        }
    }
}

pub fn latest_turn_total_usage_tokens(turns: &[MessageTurn]) -> Option<u64> {
    turns.iter().rev().find_map(|turn| {
        turn.usage.as_ref().map(|usage| {
            usage
                .input_tokens
                .saturating_add(usage.output_tokens)
                .saturating_add(usage.cache_creation_input_tokens)
                .saturating_add(usage.cache_read_input_tokens)
        })
    })
}

/// Context-window occupancy for agents whose transcripts carry ANTHROPIC-SHAPED
/// usage counters (`input_tokens` + `cache_creation_input_tokens` +
/// `cache_read_input_tokens` are the whole prompt; `output_tokens` is the reply).
///
/// Deliberately NOT [`latest_turn_total_usage_tokens`], which adds
/// `output_tokens` too: for this counter shape the reply is not resident in the
/// prompt window that produced it, so including it over-reports the gauge by
/// the last turn's output. Claude Code and Qoder both write this shape.
pub fn latest_turn_prompt_usage_tokens(turns: &[MessageTurn]) -> Option<u64> {
    turns.iter().rev().find_map(|turn| {
        turn.usage.as_ref().and_then(|usage| {
            let used = usage
                .input_tokens
                .saturating_add(usage.cache_creation_input_tokens)
                .saturating_add(usage.cache_read_input_tokens);
            (used > 0).then_some(used)
        })
    })
}

pub fn merge_context_window_stats(
    stats: Option<SessionStats>,
    used_tokens: Option<u64>,
    max_tokens: Option<u64>,
) -> Option<SessionStats> {
    if used_tokens.is_none() && max_tokens.is_none() {
        return stats;
    }

    let usage_percent = match (used_tokens, max_tokens) {
        (Some(used), Some(max)) if max > 0 => Some((used as f64 / max as f64) * 100.0),
        _ => None,
    };

    match stats {
        Some(mut s) => {
            s.context_window_used_tokens = used_tokens;
            s.context_window_max_tokens = max_tokens;
            s.context_window_usage_percent = usage_percent;
            Some(s)
        }
        None => Some(SessionStats {
            total_usage: None,
            total_tokens: None,
            total_duration_ms: 0,
            context_window_used_tokens: used_tokens,
            context_window_max_tokens: max_tokens,
            context_window_usage_percent: usage_percent,
        }),
    }
}

/// Relocate orphaned tool_result blocks to the turn that contains their matching tool_use.
///
/// After `group_into_turns` splits assistant rounds, async tool execution can cause
/// a tool_result to land in a later turn than its corresponding tool_use.
/// This post-processing step moves such orphaned results back.
pub fn relocate_orphaned_tool_results(turns: &mut Vec<MessageTurn>) {
    // Build map: tool_use_id → turn index
    let mut tool_use_turn: HashMap<String, usize> = HashMap::new();
    for (idx, turn) in turns.iter().enumerate() {
        for block in &turn.blocks {
            if let ContentBlock::ToolUse {
                tool_use_id: Some(ref id),
                ..
            } = block
            {
                tool_use_turn.insert(id.clone(), idx);
            }
        }
    }

    if tool_use_turn.is_empty() {
        return;
    }

    // Collect (source_turn, target_turn, block) for orphaned results
    let mut relocations: Vec<(usize, usize, ContentBlock)> = Vec::new();
    for (turn_idx, turn) in turns.iter().enumerate() {
        for block in &turn.blocks {
            if let ContentBlock::ToolResult {
                tool_use_id: Some(ref id),
                ..
            } = block
            {
                if let Some(&target) = tool_use_turn.get(id) {
                    if target != turn_idx {
                        relocations.push((turn_idx, target, block.clone()));
                    }
                }
            }
        }
    }

    if relocations.is_empty() {
        return;
    }

    // Build set of (turn_idx, tool_use_id) to remove
    let remove_set: HashMap<usize, Vec<String>> = {
        let mut map: HashMap<usize, Vec<String>> = HashMap::new();
        for (from, _, block) in &relocations {
            if let ContentBlock::ToolResult {
                tool_use_id: Some(ref id),
                ..
            } = block
            {
                map.entry(*from).or_default().push(id.clone());
            }
        }
        map
    };

    // Remove from source turns
    for (&turn_idx, ids) in &remove_set {
        turns[turn_idx].blocks.retain(|block| {
            if let ContentBlock::ToolResult {
                tool_use_id: Some(ref id),
                ..
            } = block
            {
                !ids.contains(id)
            } else {
                true
            }
        });
    }

    // Append to target turns
    for (_, target, block) in relocations {
        turns[target].blocks.push(block);
    }

    // Remove turns that became empty after relocation
    turns.retain(|turn| !turn.blocks.is_empty());
}

/// Convert Read tool output from numbered-line format to `{"start_line":N,"content":"..."}`.
///
/// Claude Code embeds line numbers in Read output like `   115→content`.
/// This splits on the `→` delimiter (or tab for older `cat -n` format),
/// extracts the starting line number, and returns clean content.
pub fn structurize_read_tool_output(turns: &mut [MessageTurn]) {
    let mut read_tool_ids: HashSet<String> = HashSet::new();
    for turn in turns.iter() {
        for block in &turn.blocks {
            if let ContentBlock::ToolUse {
                tool_use_id: Some(ref id),
                ref tool_name,
                ..
            } = block
            {
                let name = tool_name.to_lowercase();
                if matches!(
                    name.as_str(),
                    "read" | "read_file" | "readfile" | "read file" | "cat" | "view"
                ) {
                    read_tool_ids.insert(id.clone());
                }
            }
        }
    }

    for turn in turns.iter_mut() {
        for block in turn.blocks.iter_mut() {
            let is_read_result = matches!(
                block,
                ContentBlock::ToolResult { tool_use_id: Some(ref id), .. }
                if read_tool_ids.contains(id)
            );
            if !is_read_result {
                continue;
            }
            if let ContentBlock::ToolResult {
                ref mut output_preview,
                ..
            } = block
            {
                if let Some(ref text) = output_preview {
                    if let Some(json) = strip_numbered_lines(text) {
                        *output_preview = Some(json);
                    }
                }
            }
        }
    }
}

/// Known delimiters between line number and content.
const LINE_NUM_DELIMITERS: &[&str] = &["→", "\t"];

/// Try to split a line at a known delimiter, returning (line_number, content).
fn split_line_number(line: &str) -> Option<(u64, &str)> {
    for delim in LINE_NUM_DELIMITERS {
        if let Some(pos) = line.find(delim) {
            let prefix = line[..pos].trim();
            if let Ok(num) = prefix.parse::<u64>() {
                let content_start = pos + delim.len();
                return Some((num, &line[content_start..]));
            }
        }
    }
    None
}

/// If most lines have a recognized line-number prefix, strip them all
/// and return `{"start_line":N,"content":"clean text"}`.
pub fn strip_numbered_lines(text: &str) -> Option<String> {
    let raw_lines: Vec<&str> = text.lines().collect();
    if raw_lines.len() < 2 {
        return None;
    }

    let matched = raw_lines
        .iter()
        .filter(|l| l.is_empty() || split_line_number(l).is_some())
        .count();
    if matched < raw_lines.len() * 4 / 5 {
        return None;
    }

    let mut start_line: u64 = 1;
    let mut first = true;
    let stripped: Vec<&str> = raw_lines
        .iter()
        .map(|line| {
            if let Some((num, content)) = split_line_number(line) {
                if first {
                    start_line = num;
                    first = false;
                }
                content
            } else {
                first = false;
                *line
            }
        })
        .collect();

    Some(
        serde_json::json!({
            "start_line": start_line,
            "content": stripped.join("\n")
        })
        .to_string(),
    )
}

/// Resolve line numbers for `*** Update File` / `*** Add File` style patches.
///
/// When a hunk header is just `@@` without `-N,M +N,M`, this reads the actual
/// file from disk and matches the context lines to calculate real line numbers.
/// Falls back gracefully if the file doesn't exist or context doesn't match.
pub fn resolve_patch_line_numbers(turns: &mut [MessageTurn], cwd: Option<&str>) {
    for turn in turns.iter_mut() {
        for block in turn.blocks.iter_mut() {
            if let ContentBlock::ToolUse {
                ref tool_name,
                ref mut input_preview,
                ..
            } = block
            {
                let name = tool_name.to_lowercase();
                if !matches!(
                    name.as_str(),
                    "apply_patch" | "edit" | "patch" | "applypatch"
                ) {
                    continue;
                }
                if let Some(ref text) = input_preview {
                    if text.contains("@@\n") || text.contains("@@\r\n") {
                        if let Some(resolved) = resolve_patch_text(text, cwd) {
                            *input_preview = Some(resolved);
                        }
                    }
                }
            }
        }
    }
}

/// Resolve a single patch text, replacing bare `@@` with `@@ -N,M +N,M @@`.
pub fn resolve_patch_text(patch: &str, cwd: Option<&str>) -> Option<String> {
    let mut output = String::with_capacity(patch.len() + 256);
    let mut current_file_path: Option<String> = None;
    let mut file_lines: Option<Vec<String>> = None;
    let mut any_resolved = false;

    let lines: Vec<&str> = patch.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Detect file markers
        if line.starts_with("*** Update File: ") || line.starts_with("*** Add File: ") {
            let marker_end = if line.starts_with("*** Update File: ") {
                17
            } else {
                14
            };
            let path = line[marker_end..].trim();
            current_file_path = Some(path.to_string());
            file_lines = load_file_lines(path, cwd);
            output.push_str(line);
            output.push('\n');
            i += 1;
            continue;
        }

        // Detect bare @@ hunk header (no line numbers)
        if line == "@@" {
            if let (Some(ref fl), true) = (&file_lines, current_file_path.is_some()) {
                // Collect context lines from this hunk to find match position
                let hunk_lines = collect_hunk_lines(&lines, i + 1);
                if let Some((old_start, old_count, new_count)) = find_hunk_position(fl, &hunk_lines)
                {
                    let new_start = old_start; // same start for context-based patches
                    output.push_str(&format!(
                        "@@ -{},{} +{},{} @@\n",
                        old_start, old_count, new_start, new_count
                    ));
                    any_resolved = true;
                    i += 1;
                    continue;
                }
            }
            // Fallback: keep bare @@
            output.push_str(line);
            output.push('\n');
            i += 1;
            continue;
        }

        output.push_str(line);
        output.push('\n');
        i += 1;
    }

    if any_resolved {
        Some(output)
    } else {
        None
    }
}

/// Load file lines from disk, trying both absolute path and cwd-relative.
pub fn load_file_lines(path: &str, cwd: Option<&str>) -> Option<Vec<String>> {
    use std::fs;
    use std::path::Path;

    let p = Path::new(path);
    if p.is_absolute() {
        if let Ok(content) = fs::read_to_string(p) {
            return Some(content.lines().map(|l| l.to_string()).collect());
        }
    }
    if let Some(base) = cwd {
        let full = Path::new(base).join(path);
        if let Ok(content) = fs::read_to_string(&full) {
            return Some(content.lines().map(|l| l.to_string()).collect());
        }
    }
    None
}

/// Collect lines belonging to a hunk (until next `@@` or `*** ` marker or end).
fn collect_hunk_lines<'a>(lines: &'a [&'a str], start: usize) -> Vec<&'a str> {
    let mut result = Vec::new();
    for &line in &lines[start..] {
        if line == "@@" || line.starts_with("*** ") {
            break;
        }
        result.push(line);
    }
    result
}

/// Find where a hunk's context lines match in the file, returning (start_line, old_count, new_count).
/// `start_line` is 1-based.
///
/// The file on disk may be in either pre-patch or post-patch state, and may
/// have been further modified. We try three strategies in order:
/// 1. Contiguous match of context+added lines (post-patch file, no further edits)
/// 2. Contiguous match of context+deleted lines (pre-patch file)
/// 3. Subsequence match of context-only lines (file has been further modified)
fn find_hunk_position(file_lines: &[String], hunk_lines: &[&str]) -> Option<(usize, usize, usize)> {
    let mut old_count = 0usize;
    let mut new_count = 0usize;
    for hl in hunk_lines {
        if hl.starts_with(' ') {
            old_count += 1;
            new_count += 1;
        } else if hl.starts_with('-') {
            old_count += 1;
        } else if hl.starts_with('+') {
            new_count += 1;
        }
    }

    // Strategy 1: contiguous match of context+added (post-patch)
    let new_view: Vec<&str> = hunk_lines
        .iter()
        .filter(|l| l.starts_with(' ') || l.starts_with('+'))
        .map(|l| &l[1..])
        .collect();
    if let Some(pos) = find_contiguous(file_lines, &new_view) {
        return Some((pos + 1, old_count, new_count));
    }

    // Strategy 2: contiguous match of context+deleted (pre-patch)
    let old_view: Vec<&str> = hunk_lines
        .iter()
        .filter(|l| l.starts_with(' ') || l.starts_with('-'))
        .map(|l| &l[1..])
        .collect();
    if let Some(pos) = find_contiguous(file_lines, &old_view) {
        return Some((pos + 1, old_count, new_count));
    }

    // Strategy 3: subsequence match of context-only lines (file further modified)
    let ctx_only: Vec<&str> = hunk_lines
        .iter()
        .filter(|l| l.starts_with(' '))
        .map(|l| &l[1..])
        .collect();
    if let Some(pos) = find_subsequence(file_lines, &ctx_only) {
        return Some((pos + 1, old_count, new_count));
    }

    None
}

/// Find contiguous `view` lines in `file_lines`. Returns 0-based start index.
fn find_contiguous(file_lines: &[String], view: &[&str]) -> Option<usize> {
    if view.is_empty() || view.len() > file_lines.len() {
        return None;
    }
    let first = view[0];
    for i in 0..=(file_lines.len() - view.len()) {
        if file_lines[i].as_str() != first {
            continue;
        }
        if view
            .iter()
            .enumerate()
            .all(|(j, v)| file_lines[i + j].as_str() == *v)
        {
            return Some(i);
        }
    }
    None
}

/// Find `needles` as an ordered subsequence in `file_lines` within a small window.
/// Returns 0-based index of the first needle's position.
fn find_subsequence(file_lines: &[String], needles: &[&str]) -> Option<usize> {
    if needles.is_empty() {
        return None;
    }
    let first = needles[0];
    for start in 0..file_lines.len() {
        if file_lines[start].as_str() != first {
            continue;
        }
        let mut cursor = start + 1;
        let mut all_found = true;
        for &needle in &needles[1..] {
            // Allow up to 10 lines gap between consecutive context lines
            let limit = std::cmp::min(cursor + 10, file_lines.len());
            match file_lines[cursor..limit]
                .iter()
                .position(|fl| fl.as_str() == needle)
            {
                Some(offset) => cursor = cursor + offset + 1,
                None => {
                    all_found = false;
                    break;
                }
            }
        }
        if all_found {
            return Some(start);
        }
    }
    None
}

/// Extract the last path component as the folder name.
pub fn folder_name_from_path(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Normalize a filesystem path string for tolerant cross-platform comparison.
/// This intentionally does not hit the filesystem (no canonicalize), and only
/// normalizes separators/casing differences that commonly break exact matching.
pub fn normalize_path_for_matching(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");

    #[cfg(target_os = "windows")]
    {
        if let Some(stripped) = normalized.strip_prefix("//?/") {
            normalized = stripped.to_string();
        }
        normalized = normalized.to_ascii_lowercase();
    }

    while normalized.ends_with('/') {
        if normalized == "/" {
            break;
        }
        // Keep Windows drive root such as "c:/" intact.
        if normalized.len() == 3
            && normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes().get(2) == Some(&b'/')
        {
            break;
        }
        normalized.pop();
    }

    normalized
}

pub fn path_eq_for_matching(left: &str, right: &str) -> bool {
    normalize_path_for_matching(left) == normalize_path_for_matching(right)
}

#[cfg(test)]
mod route_sanitizer_tests {

    use super::{AgentParser, ParseError, RecoveryQuery, RouteSanitized};
    use crate::acp::agent_mentions::append_agent_routes;
    use crate::acp::types::PromptInputBlock;
    use crate::models::{
        AgentType, ContentBlock, ConversationDetail, ConversationSummary, MessageTurn, TurnRole,
    };

    /// The exact bytes `append_agent_routes` puts on the wire — the same thing
    /// every MCP-capable agent then persists into its own transcript.
    fn routing_frame(agent_wire: &str) -> String {
        let mut blocks = vec![PromptInputBlock::Text {
            text: format!("ask [@A](codeg://agent/{agent_wire}) to help"),
        }];
        append_agent_routes(&mut blocks, true);
        match &blocks[1] {
            PromptInputBlock::Text { text } => text.clone(),
            _ => unreachable!("the routing block is text"),
        }
    }

    fn turn(role: TurnRole, text: &str) -> MessageTurn {
        MessageTurn {
            id: format!("{role:?}-{}", text.len()),
            role,
            blocks: vec![ContentBlock::Text { text: text.into() }],
            timestamp: "2026-03-01T10:00:00Z".parse().expect("valid timestamp"),
            usage: None,
            duration_ms: None,
            model: None,
            reasoning_effort: None,
            completed_at: None,
            outcome: None,
            autonomous_origin: None,
            generation_ms: None,
            generation_tokens: None,
        }
    }

    fn summary(title: Option<&str>, message_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: "conv-1".into(),
            agent_type: AgentType::ClaudeCode,
            folder_path: None,
            folder_name: None,
            title: title.map(str::to_string),
            started_at: "2026-03-01T10:00:00Z".parse().expect("valid timestamp"),
            ended_at: None,
            message_count,
            model: None,
            git_branch: None,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        }
    }

    struct Fixture {
        summary: ConversationSummary,
        turns: Vec<MessageTurn>,
    }

    impl AgentParser for Fixture {
        fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
            Ok(vec![self.summary.clone()])
        }
        fn get_conversation(&self, _id: &str) -> Result<ConversationDetail, ParseError> {
            Ok(ConversationDetail {
                summary: self.summary.clone(),
                turns: self.turns.clone(),
                session_stats: None,
                transcript_watermark: None,
            })
        }

        fn recover_conversation(
            &self,
            _query: &RecoveryQuery<'_>,
            accept: &dyn Fn(&ConversationSummary) -> bool,
        ) -> Result<Option<ConversationDetail>, ParseError> {
            if !accept(&self.summary) {
                return Ok(None);
            }
            self.get_conversation(&self.summary.id).map(Some)
        }
    }

    fn sanitized(fixture: Fixture) -> ConversationDetail {
        RouteSanitized(Box::new(fixture))
            .get_conversation("conv-1")
            .expect("fixture parses")
    }

    #[test]
    fn stale_session_recovery_is_forwarded_and_sanitized() {
        let frame = routing_frame("gemini");
        let visible = "recover this session";
        let parser = RouteSanitized(Box::new(Fixture {
            summary: summary(Some(&format!("{visible}\n{frame}")), 2),
            turns: vec![
                turn(TurnRole::User, &format!("{visible}\n{frame}")),
                turn(TurnRole::Assistant, "recovered"),
            ],
        }));
        let query = RecoveryQuery {
            cwd: "/tmp/recovered",
            approx: "2026-03-01T10:00:00Z".parse().expect("valid timestamp"),
            max_skew: chrono::Duration::minutes(5),
            ambiguity: chrono::Duration::seconds(1),
        };

        let detail = parser
            .recover_conversation(&query, &|_| true)
            .expect("recovery parses")
            .expect("wrapped parser recovery is forwarded");

        assert_eq!(detail.summary.title.as_deref(), Some(visible));
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == visible
        ));
    }

    /// A title is capped by its parser BEFORE it reaches the decorator, and the
    /// cap has no idea where the frame starts. A short first prompt puts the cut
    /// inside the frame, leaving an opening separator and half a descriptor with
    /// no closing one — which the whole-frame strip pass will not touch.
    #[test]
    fn a_title_truncated_mid_frame_leaks_no_route_metadata() {
        let frame = routing_frame("antigravity");
        // Exactly what `acp_native::first_prompt_title` builds: the prompt's
        // text blocks joined with no separator, then capped at 80.
        let truncated = super::truncate_str(format!("hi{frame}").trim(), 80);
        assert!(
            truncated.contains('\u{001e}')
                && !crate::acp::agent_mentions::contains_internal_agent_routes(&truncated),
            "fixture must straddle the frame, or it proves nothing"
        );

        let fixture = || Fixture {
            summary: summary(Some(&truncated), 1),
            turns: vec![turn(TurnRole::User, &format!("hi\n{frame}"))],
        };
        assert_eq!(sanitized(fixture()).summary.title.as_deref(), Some("hi"));
        // The sidebar reads the list path, which never sees the turns.
        let listed = RouteSanitized(Box::new(fixture()))
            .list_conversations()
            .expect("fixture parses");
        assert_eq!(listed[0].title.as_deref(), Some("hi"));
    }

    #[test]
    fn frames_are_stripped_from_any_agents_history_not_just_codex() {
        // The whole point of the shared decorator: this fixture stands in for
        // claude / gemini / opencode / … , none of which know about the frame.
        let frame = routing_frame("antigravity");
        let visible = "ask [@A](codeg://agent/antigravity) to help";
        let detail = sanitized(Fixture {
            summary: summary(Some(&format!("{visible}\n{frame}")), 2),
            turns: vec![
                turn(TurnRole::User, &format!("{visible}\n{frame}")),
                turn(TurnRole::Assistant, "on it"),
            ],
        });

        assert_eq!(detail.summary.title.as_deref(), Some(visible));
        assert_eq!(detail.turns.len(), 2);
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == visible
        ));
        assert_eq!(detail.summary.message_count, 2);
    }

    /// The shape `append_agent_routes` ACTUALLY produces: the frame is its own
    /// prompt block, so every parser that keeps one block per recorded text item
    /// (claude's `extract_user_content`, `acp_native`'s `prompt_blocks`) hands
    /// back two. Emptying the second in place used to leave a zero-height text
    /// part that the transcript's `space-y-4` stack still gave a full gap — the
    /// blank band under an `@`-mention bubble reopened from history.
    #[test]
    fn a_frame_in_its_own_block_leaves_no_empty_block_behind() {
        let visible = "ask [@A](codeg://agent/claude_code) to help";
        let mut user = turn(TurnRole::User, visible);
        user.blocks.push(ContentBlock::Text {
            text: routing_frame("claude_code"),
        });
        let detail = sanitized(Fixture {
            summary: summary(Some(visible), 2),
            turns: vec![user, turn(TurnRole::Assistant, "on it")],
        });

        assert_eq!(detail.turns.len(), 2);
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == visible
        ));
        assert_eq!(detail.summary.message_count, 2);
    }

    /// An agent that joins the prompt's text blocks with a BLANK line leaves the
    /// strip one newline to spare (it absorbs a single adjacent one), and
    /// `whitespace-pre-wrap` paints the survivor as an empty line under the
    /// message — same symptom, different transcript shape.
    #[test]
    fn a_blank_line_before_the_frame_is_not_left_behind() {
        let visible = "ask [@A](codeg://agent/codex) to help";
        let frame = routing_frame("codex");
        let detail = sanitized(Fixture {
            summary: summary(Some(visible), 1),
            turns: vec![turn(TurnRole::User, &format!("{visible}\n\n{frame}"))],
        });

        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == visible
        ));
    }

    /// Antigravity does not persist the prompt verbatim: its ACP server joins
    /// the text blocks with a space and replaces each separator with one, so the
    /// trajectory holds ONE block of prose with the frame trailing it and no
    /// separator anywhere. The whole frame used to render inside the user's
    /// bubble when such a session was reopened — and, sliced by the title cap,
    /// inside the sidebar title as well.
    #[test]
    fn a_frame_an_agent_stored_without_separators_leaves_neither_prose_nor_title() {
        let visible = "ask [@A](codeg://agent/antigravity) to help";
        let frame = routing_frame("antigravity");
        // ` ` block join + each separator rewritten to ` `.
        let persisted = format!("{visible} {}", frame.replace('\u{001e}', " "));
        assert!(
            !persisted.contains('\u{001e}'),
            "fixture must lose its separators"
        );

        // The title the parser hands over: folded, then capped mid-frame — the
        // cap is 100 chars and the body alone runs past 500.
        let capped = super::title_from_user_text(&persisted);
        assert!(
            capped.contains("codeg_internal_agent_routes"),
            "fixture must straddle the frame, or the cut proves nothing"
        );
        let detail = sanitized(Fixture {
            summary: summary(Some(&capped), 1),
            turns: vec![turn(TurnRole::User, &persisted)],
        });

        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == visible
        ));
        assert_eq!(detail.summary.title.as_deref(), Some("ask @A to help"));
    }

    /// The 100-char cap can land INSIDE the descriptor prefix rather than
    /// before it, for any first prompt whose prose length falls in a 37-wide
    /// band. A separator can never be halved that way, so this shape only
    /// exists for the agents that rewrite the separators away.
    #[test]
    fn a_title_capped_halfway_through_the_marker_still_leaks_nothing() {
        let prose = format!(
            "{} ask [@A](codeg://agent/antigravity) to help",
            "x".repeat(66)
        );
        let frame = routing_frame("antigravity");
        let persisted = format!("{prose} {}", frame.replace('\u{001e}', " "));

        let capped = super::title_from_user_text(&persisted);
        let marker = "{\"kind\":\"codeg_internal_agent_routes\"";
        assert!(
            capped.contains("{\"kind\":\"codeg") && !capped.contains(marker),
            "the cap must land INSIDE the marker, or this repeats the previous \
             test — got {capped:?}"
        );

        let detail = sanitized(Fixture {
            summary: summary(Some(&capped), 1),
            turns: vec![turn(TurnRole::User, &persisted)],
        });
        assert_eq!(
            detail.summary.title.as_deref(),
            Some(format!("{} ask @A to help", "x".repeat(66)).as_str())
        );
    }

    #[test]
    fn an_empty_block_the_parser_produced_itself_is_left_in_place() {
        // The drop is scoped to blocks that HELD a frame. An empty text block
        // from anywhere else is the parser's business, not this decorator's, and
        // silently pruning it would hide a bug rather than fix one.
        let mut user = turn(TurnRole::User, "real prompt");
        user.blocks.push(ContentBlock::Text {
            text: String::new(),
        });
        let detail = sanitized(Fixture {
            summary: summary(Some("real prompt"), 1),
            turns: vec![user],
        });

        assert_eq!(detail.turns[0].blocks.len(), 2);
    }

    #[test]
    fn a_turn_whose_only_prose_was_the_frame_keeps_its_image() {
        // Dropping the emptied block must not take the turn with it: an image
        // pasted alongside an `@`-mention is the whole message.
        let mut user = turn(TurnRole::User, &routing_frame("codex"));
        user.blocks.push(ContentBlock::Image {
            data: "QUJD".into(),
            mime_type: "image/png".into(),
            uri: None,
        });
        let detail = sanitized(Fixture {
            summary: summary(None, 1),
            turns: vec![user],
        });

        assert_eq!(detail.turns.len(), 1);
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Image { .. }]
        ));
        assert_eq!(detail.summary.message_count, 1);
    }

    #[test]
    fn a_route_only_user_turn_is_dropped_and_uncounted() {
        // Some adapters persist each ACP text block as its own record; the
        // transport-only one must not render as a phantom turn.
        let detail = sanitized(Fixture {
            summary: summary(Some("real prompt"), 3),
            turns: vec![
                turn(TurnRole::User, "real prompt"),
                turn(TurnRole::User, &routing_frame("codex")),
                turn(TurnRole::Assistant, "done"),
            ],
        });

        assert_eq!(detail.turns.len(), 2);
        assert_eq!(detail.summary.message_count, 2);
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == "real prompt"
        ));
    }

    #[test]
    fn assistant_turns_and_look_alike_user_prose_are_left_alone() {
        // Only a complete frame that re-renders byte-for-byte is removed, so
        // prose that merely resembles one stays verbatim, on both roles. The
        // separator is scrubbed from every prompt at ingress, so prose a user
        // can actually type is the tag text WITHOUT it.
        let look_alike =
            "see <codeg_internal_agent_routes version=\"2\">note</codeg_internal_agent_routes>";
        // Each frame carries its own nonce, so capture ONE and compare to it.
        let echoed = routing_frame("codex");
        let detail = sanitized(Fixture {
            summary: summary(Some(look_alike), 2),
            turns: vec![
                turn(TurnRole::User, look_alike),
                turn(TurnRole::Assistant, &echoed),
            ],
        });

        assert_eq!(detail.summary.title.as_deref(), Some(look_alike));
        assert_eq!(detail.turns.len(), 2);
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == look_alike
        ));
        assert!(matches!(
            detail.turns[1].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == &echoed
        ));
    }

    /// The cut is deliberately asymmetric, so pin both halves.
    #[test]
    fn only_a_title_is_cut_at_a_dangling_separator() {
        // A parser caps a title but never a turn's text, so a lone separator is
        // evidence of a sliced frame in the first case and not in the second.
        let dangling = "see \u{001e}<codeg_internal_agent_routes version=\"2\">no";
        let detail = sanitized(Fixture {
            summary: summary(Some(dangling), 1),
            turns: vec![turn(TurnRole::User, dangling)],
        });

        assert_eq!(detail.summary.title.as_deref(), Some("see"));
        assert!(matches!(
            detail.turns[0].blocks.as_slice(),
            [ContentBlock::Text { text }] if text == dangling
        ));
    }

    #[test]
    fn a_title_that_was_only_a_frame_falls_back_to_none() {
        let detail = sanitized(Fixture {
            summary: summary(Some(&routing_frame("codex")), 1),
            turns: vec![turn(TurnRole::Assistant, "hi")],
        });
        assert_eq!(detail.summary.title, None);

        let listed = RouteSanitized(Box::new(Fixture {
            summary: summary(Some(&routing_frame("codex")), 1),
            turns: Vec::new(),
        }))
        .list_conversations()
        .expect("fixture lists");
        assert_eq!(listed[0].title, None);
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{
        external_transcript_sources_for_runtime_env, fold_reference_links,
        infer_context_window_max_tokens, is_safe_subagent_id, latest_turn_total_usage_tokens,
        merge_context_window_stats, path_eq_for_matching, sanitize_user_blocks,
        select_unique_recovery_match, title_from_user_text, visible_user_text, RecoveryQuery,
    };
    use crate::models::{
        AgentType, ContentBlock, ConversationSummary, MessageTurn, SessionStats, TurnRole,
        TurnUsage,
    };

    fn test_terminal_context() -> String {
        test_terminal_context_for("PowerShell 7", "powershell", "PowerShell")
    }

    #[test]
    fn deepseek_external_source_is_omitted_when_child_home_is_unknown() {
        #[cfg(windows)]
        let home_key = "USERPROFILE";
        #[cfg(not(windows))]
        let home_key = "HOME";

        for home in ["", "relative-child-home"] {
            let runtime_env = std::collections::BTreeMap::from([
                ("DEEPSEEK_ACP_SESSIONS_ROOT".to_string(), String::new()),
                ("DSH_HOME".to_string(), String::new()),
                (home_key.to_string(), home.to_string()),
            ]);
            assert!(
                external_transcript_sources_for_runtime_env(&runtime_env)
                    .iter()
                    .all(|source| source.agent != "deepseek"),
                "an unresolved child home must not fall back to Codeg's profile"
            );
        }

        #[cfg(windows)]
        {
            let runtime_env = std::collections::BTreeMap::from([
                ("DEEPSEEK_ACP_SESSIONS_ROOT".to_string(), String::new()),
                ("DSH_HOME".to_string(), String::new()),
                ("HOMEDRIVE".to_string(), "Z:".to_string()),
                ("HOMEPATH".to_string(), r"\relocated-home".to_string()),
            ]);
            assert!(
                external_transcript_sources_for_runtime_env(&runtime_env)
                    .iter()
                    .all(|source| source.agent != "deepseek"),
                "a relocated Windows home pair must not use Codeg's USERPROFILE"
            );
        }
    }

    #[test]
    fn deepseek_external_source_keeps_explicit_runtime_roots_without_child_home() {
        #[cfg(windows)]
        let home_key = "USERPROFILE";
        #[cfg(not(windows))]
        let home_key = "HOME";
        #[cfg(windows)]
        let explicit_sessions_root = r"C:\deepseek-explicit-sessions";
        #[cfg(not(windows))]
        let explicit_sessions_root = "/tmp/deepseek-explicit-sessions";
        #[cfg(windows)]
        let absolute_dsh_root = r"C:\deepseek-absolute-home";
        #[cfg(not(windows))]
        let absolute_dsh_root = "/tmp/deepseek-absolute-home";

        let explicit_sessions = std::collections::BTreeMap::from([
            (
                "DEEPSEEK_ACP_SESSIONS_ROOT".to_string(),
                explicit_sessions_root.to_string(),
            ),
            ("DSH_HOME".to_string(), String::new()),
            (home_key.to_string(), String::new()),
        ]);
        let sessions_source = external_transcript_sources_for_runtime_env(&explicit_sessions)
            .into_iter()
            .find(|source| source.agent == "deepseek")
            .expect("an explicit sessions root remains resolvable");
        assert_eq!(
            sessions_source.root,
            std::path::PathBuf::from(explicit_sessions_root)
        );

        let absolute_dsh = std::collections::BTreeMap::from([
            ("DEEPSEEK_ACP_SESSIONS_ROOT".to_string(), String::new()),
            ("DSH_HOME".to_string(), absolute_dsh_root.to_string()),
            (home_key.to_string(), String::new()),
        ]);
        let dsh_source = external_transcript_sources_for_runtime_env(&absolute_dsh)
            .into_iter()
            .find(|source| source.agent == "deepseek")
            .expect("an absolute DSH_HOME remains resolvable");
        assert_eq!(
            dsh_source.root,
            std::path::PathBuf::from(absolute_dsh_root).join("sessions")
        );
    }

    fn test_terminal_context_for(
        selected_shell: &str,
        dialect: &str,
        syntax_label: &str,
    ) -> String {
        if dialect == "custom" {
            format!(
                "<codeg_terminal_context version=\"1\">\n\
Selected shell: {selected_shell}\n\
Dialect: custom\n\
Generate shell command lines using the selected custom shell's syntax.\n\
ACP command+args requests may still execute directly.\n\
This context is authoritative for the current connection and supersedes\n\
earlier terminal context records.\n\
</codeg_terminal_context>"
            )
        } else {
            format!(
                "<codeg_terminal_context version=\"1\">\n\
Selected shell: {selected_shell}\n\
Dialect: {dialect}\n\
Generate shell command lines using {syntax_label} syntax.\n\
ACP command+args requests may still execute directly.\n\
This context is authoritative for the current connection and supersedes\n\
earlier terminal context records.\n\
</codeg_terminal_context>"
            )
        }
    }

    #[test]
    fn strips_context_only_and_appended_context() {
        let context = test_terminal_context();
        assert_eq!(visible_user_text(&context), None);
        assert_eq!(
            visible_user_text(&format!("real prompt\n\n{context}")),
            Some("real prompt".to_string())
        );
        assert_eq!(
            visible_user_text(&format!("real prompt{context}")),
            Some("real prompt".to_string())
        );
    }

    #[test]
    fn strips_multiple_complete_superseded_contexts() {
        let old = test_terminal_context_for("Command Prompt", "cmd", "CMD");
        let new = test_terminal_context_for("PowerShell 7", "powershell", "PowerShell");
        assert_eq!(
            visible_user_text(&format!("{old}\nreal prompt\n{new}")),
            Some("real prompt".to_string())
        );
    }

    #[test]
    fn preserves_malformed_partial_and_ordinary_xml() {
        for text in [
            "<codeg_terminal_context version=\"1\">partial",
            "<codeg_terminal_context version=\"2\">user text</codeg_terminal_context>",
            "<codeg_terminal_context version=\"1\">user-authored XML</codeg_terminal_context>",
            "<terminal>ordinary user XML</terminal>",
        ] {
            assert_eq!(visible_user_text(text), Some(text.to_string()));
        }
    }

    #[test]
    fn mandatory_route_filter_drops_only_pure_column_zero_blocks() {
        let route_a = "Codeg mandatory delegation route: profile_id=\"a\"";
        let route_b = "Codeg mandatory delegation route: profile_id=\"b\"";

        assert_eq!(visible_user_text(route_a), None);
        assert_eq!(
            visible_user_text(&format!("{route_a}  \n  \n{route_b}\t")),
            None
        );

        let mixed = format!("{route_a}\nPlease fix the bug");
        assert_eq!(visible_user_text(&mixed), Some(mixed.clone()));
        let mentioned = format!("Please quote {route_a}");
        assert_eq!(visible_user_text(&mentioned), Some(mentioned.clone()));
        let indented = format!("  {route_a}");
        assert_eq!(visible_user_text(&indented), Some(route_a.to_string()));
    }

    #[test]
    fn sanitize_user_blocks_drops_routes_but_keeps_prose_and_media() {
        let mut blocks = vec![
            ContentBlock::Text {
                text: "Codeg mandatory delegation route: profile_id=\"a\"".into(),
            },
            ContentBlock::Image {
                data: "QUJD".into(),
                mime_type: "image/png".into(),
                uri: Some("clipboard://image.png".into()),
            },
            ContentBlock::Text {
                text: "Please inspect this".into(),
            },
        ];
        assert!(sanitize_user_blocks(&mut blocks));
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], ContentBlock::Image { .. }));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Text { text } if text == "Please inspect this"
        ));

        let mut route_only = vec![ContentBlock::Text {
            text: "Codeg mandatory delegation route: profile_id=\"a\"".into(),
        }];
        assert!(!sanitize_user_blocks(&mut route_only));
        assert!(route_only.is_empty());
    }

    #[test]
    fn safe_subagent_id_accepts_plain_ids_and_rejects_traversal() {
        // Real CodeBuddy / Claude sub-agent ids are plain tokens.
        assert!(is_safe_subagent_id("agent-cdd7c1ea"));
        assert!(is_safe_subagent_id("agent-test01"));
        // Every escape vector is rejected — including the Windows-only drive
        // colon that the old `/ \\ ..`-only guard let through.
        for hostile in [
            "",
            "..",
            "../../etc/passwd",
            "a/b",
            "a\\b",
            "C:evil",
            "C:\\Windows\\System32",
            "a\0b",
        ] {
            assert!(
                !is_safe_subagent_id(hostile),
                "expected rejection for {hostile:?}"
            );
        }
    }

    #[test]
    fn fold_reference_links_reduces_links_to_labels() {
        // Plain prose is untouched.
        assert_eq!(fold_reference_links("hello world"), "hello world");
        // A file link folds to its label; surrounding text is preserved.
        assert_eq!(
            fold_reference_links("看看 [README.md](file:///Users/x/README.md) 这是什么"),
            "看看 README.md 这是什么"
        );
        // codeg:// links fold too; an agent mention keeps its `@`.
        assert_eq!(
            fold_reference_links("调用 [@Codex CLI](codeg://agent/codex) 执行"),
            "调用 @Codex CLI 执行"
        );
        // Multiple links in one string.
        assert_eq!(
            fold_reference_links("compare [a.ts](file:///a.ts) and [b.ts](file:///b.ts)"),
            "compare a.ts and b.ts"
        );
    }

    #[test]
    fn fold_reference_links_handles_escapes_and_angle_destinations() {
        // A `<…>`-wrapped destination (spaces/parens in the path) still folds.
        assert_eq!(
            fold_reference_links("[report (1).pdf](<file:///tmp/report (1).pdf>)"),
            "report (1).pdf"
        );
        // Escaped punctuation in the label is unescaped.
        assert_eq!(fold_reference_links("[a\\]b\\(c](file:///x)"), "a]b(c");
        // A balanced nested-bracket label closes at the outer `]`.
        assert_eq!(fold_reference_links("[a [b]](https://x)"), "a [b]");
        // A later link is recovered after a stray/unbalanced `[`.
        assert_eq!(fold_reference_links("[a [b](url)"), "[a b");
    }

    #[test]
    fn fold_reference_links_matches_js_whitespace_class() {
        // Parity with the frontend `foldReferenceLinks`, whose destination scan
        // uses ECMAScript `/\s/` rather than Rust's `char::is_whitespace()`. The
        // two classes differ on exactly these code points (verified against the
        // TS module): U+FEFF (BOM) and U+00A0 (NBSP) ARE JS whitespace, so a bare
        // destination containing them is malformed and the text stays raw…
        assert_eq!(
            fold_reference_links("[a](foo\u{FEFF}bar)"),
            "[a](foo\u{FEFF}bar)"
        );
        assert_eq!(
            fold_reference_links("[a](foo\u{00A0}bar)"),
            "[a](foo\u{00A0}bar)"
        );
        // …while U+0085 (NEL) is NOT JS whitespace, so it is an ordinary
        // destination char and the link folds (Rust's is_whitespace would have
        // wrongly rejected it).
        assert_eq!(fold_reference_links("[a](foo\u{0085}bar)"), "a");
    }

    #[test]
    fn fold_reference_links_leaves_malformed_fragments_raw() {
        // An unterminated link (no closing `)`) is left verbatim — exactly the
        // truncated-title shape this fix keeps from ever being stored.
        assert_eq!(
            fold_reference_links("[oops no close](file:///x"),
            "[oops no close](file:///x"
        );
        // An empty-label `[](x)` is not a link.
        assert_eq!(fold_reference_links("[](x)"), "[](x)");
        // A bare destination with an unescaped space is malformed.
        assert_eq!(fold_reference_links("[a](foo bar)"), "[a](foo bar)");
    }

    #[test]
    fn title_from_user_text_folds_before_truncating() {
        // The regression: a long percent-encoded file mention used to be
        // truncated mid-destination into an unterminable `[label](file://…`
        // fragment. Folding first yields the short, clean filename.
        let long_path = "%E5%85%A8".repeat(40); // > 100 chars when raw
        let raw = format!("[全天候运维.xlsx](file:///Users/xggz/Desktop/{long_path}.xlsx)");
        assert!(raw.chars().count() > 100, "fixture must exceed the cap");
        assert_eq!(title_from_user_text(&raw), "全天候运维.xlsx");
    }

    #[test]
    fn title_from_user_text_still_caps_plain_prose() {
        let long = "x".repeat(250);
        let title = title_from_user_text(&long);
        assert_eq!(title.chars().count(), 103); // 100 + "..."
        assert!(title.ends_with("..."));
    }

    #[test]
    fn infers_model_context_limits() {
        assert_eq!(
            infer_context_window_max_tokens(Some("claude-sonnet-4-6")),
            Some(200_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("gemini-2.5-pro")),
            Some(1_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("claude-sonnet-4-6 [1.5M]")),
            Some(1_500_000)
        );
        // The 1M lane also travels as a bare id token (`-1m`), which is how the
        // SDK — and therefore every agent that records the resolved id — spells
        // it. `\b1m\b` must not fire on an embedded run like `10m`.
        assert_eq!(
            infer_context_window_max_tokens(Some("claude-opus-4-6-1m")),
            Some(1_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("my-gateway/claude-opus-4-6-1m")),
            Some(1_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("claude-opus-4-6-10m-preview")),
            Some(200_000)
        );
        // Grok context windows per x.ai docs: grok-4.5 / grok-4.6 = 500K,
        // grok-4.3 / grok-4.20 = 1M, the coding/build models = 256K
        // (grok-code-fast-1 despite "fast"), the general -fast variants = 2M,
        // and any unknown grok model falls back to the conservative 256K.
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-4.5")),
            Some(500_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-4.6")),
            Some(500_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-4.3")),
            Some(1_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-4.20-0309-reasoning")),
            Some(1_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-build-0.1")),
            Some(256_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-code-fast-1")),
            Some(256_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-4-fast")),
            Some(2_000_000)
        );
        assert_eq!(
            infer_context_window_max_tokens(Some("grok-7-experimental")),
            Some(256_000)
        );
        assert_eq!(infer_context_window_max_tokens(Some("unknown-model")), None);
    }

    #[test]
    fn picks_latest_turn_usage_total_tokens() {
        let timestamp = Utc::now();
        let turns = vec![
            MessageTurn {
                id: "turn-0".to_string(),
                role: TurnRole::Assistant,
                blocks: vec![],
                timestamp,
                usage: Some(TurnUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_creation_input_tokens: 30,
                    cache_read_input_tokens: 40,
                }),
                duration_ms: None,
                model: None,
                reasoning_effort: None,
                completed_at: None,
                outcome: None,
                autonomous_origin: None,
                generation_ms: None,
                generation_tokens: None,
            },
            MessageTurn {
                id: "turn-1".to_string(),
                role: TurnRole::Assistant,
                blocks: vec![],
                timestamp,
                usage: Some(TurnUsage {
                    input_tokens: 11,
                    output_tokens: 21,
                    cache_creation_input_tokens: 31,
                    cache_read_input_tokens: 41,
                }),
                duration_ms: None,
                model: None,
                reasoning_effort: None,
                completed_at: None,
                outcome: None,
                autonomous_origin: None,
                generation_ms: None,
                generation_tokens: None,
            },
        ];

        assert_eq!(latest_turn_total_usage_tokens(&turns), Some(104));
    }

    #[test]
    fn merges_context_window_stats() {
        let merged = merge_context_window_stats(None, Some(1500), Some(3000))
            .expect("context stats should exist");
        assert_eq!(merged.context_window_used_tokens, Some(1500));
        assert_eq!(merged.context_window_max_tokens, Some(3000));
        assert!(merged.total_usage.is_none());
        let percent = merged
            .context_window_usage_percent
            .expect("usage percent should exist");
        assert!((percent - 50.0).abs() < f64::EPSILON);

        let existing = Some(SessionStats {
            total_usage: Some(TurnUsage {
                input_tokens: 1,
                output_tokens: 2,
                cache_creation_input_tokens: 3,
                cache_read_input_tokens: 4,
            }),
            total_tokens: Some(10),
            total_duration_ms: 100,
            context_window_used_tokens: None,
            context_window_max_tokens: None,
            context_window_usage_percent: None,
        });
        let merged_existing =
            merge_context_window_stats(existing, Some(200), Some(1000)).expect("merged");
        assert_eq!(merged_existing.total_tokens, Some(10));
        assert_eq!(merged_existing.context_window_used_tokens, Some(200));
        assert_eq!(merged_existing.context_window_max_tokens, Some(1000));
    }

    #[test]
    fn path_matching_handles_separator_differences() {
        assert!(path_eq_for_matching(
            "/Users/demo/workspace/codeg",
            "/Users/demo/workspace/codeg/"
        ));
        assert!(path_eq_for_matching(
            "C:\\Users\\demo\\workspace\\codeg",
            "C:/Users/demo/workspace/codeg"
        ));
    }

    fn recovery_summary(
        id: &str,
        cwd: &str,
        started_at: chrono::DateTime<Utc>,
        ended_at: Option<chrono::DateTime<Utc>>,
    ) -> ConversationSummary {
        ConversationSummary {
            id: id.to_string(),
            agent_type: AgentType::Cline,
            folder_path: Some(cwd.to_string()),
            folder_name: Some("app".into()),
            title: Some(id.to_string()),
            started_at,
            ended_at,
            message_count: 1,
            model: None,
            git_branch: None,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        }
    }

    fn recovery_query<'a>(cwd: &'a str, approx: chrono::DateTime<Utc>) -> RecoveryQuery<'a> {
        RecoveryQuery {
            cwd,
            approx,
            max_skew: chrono::Duration::minutes(5),
            ambiguity: chrono::Duration::seconds(60),
        }
    }

    #[test]
    fn recovery_rejects_when_best_skew_exceeds_five_minutes() {
        let t0 = Utc::now();
        let far = recovery_summary("far", "/tmp/app", t0 + chrono::Duration::minutes(6), None);
        let query = recovery_query("/tmp/app", t0);
        assert!(
            select_unique_recovery_match(std::slice::from_ref(&far), &query, &|_| true).is_none(),
            "best skew beyond 5 minutes must fail closed"
        );
    }

    #[test]
    fn recovery_rejects_when_two_candidates_within_sixty_seconds() {
        let t0 = Utc::now();
        let a = recovery_summary("a", "/tmp/app", t0, None);
        let b = recovery_summary("b", "/tmp/app", t0 + chrono::Duration::seconds(30), None);
        let query = recovery_query("/tmp/app", t0);
        assert!(
            select_unique_recovery_match(&[a, b], &query, &|_| true).is_none(),
            "two remaining candidates within 60s must fail closed"
        );
    }

    #[test]
    fn recovery_selects_unique_candidate_inside_five_minutes() {
        let t0 = Utc::now();
        let cwd = "/tmp/app";
        let only = recovery_summary("hit", cwd, t0 + chrono::Duration::minutes(2), None);
        let query = recovery_query(cwd, t0);
        assert_eq!(
            select_unique_recovery_match(std::slice::from_ref(&only), &query, &|_| true)
                .map(|s| s.id.as_str()),
            Some("hit")
        );

        let slash = recovery_summary("win", r"D:\tmp\app", t0, None);
        let query_win = recovery_query("D:/tmp/app", t0);
        assert_eq!(
            select_unique_recovery_match(std::slice::from_ref(&slash), &query_win, &|_| true)
                .map(|s| s.id.as_str()),
            Some("win"),
            "cwd matching must treat backslash and forward slash as the same path"
        );

        let mut ended_near = recovery_summary("ended", cwd, t0 - chrono::Duration::hours(1), None);
        ended_near.ended_at = Some(t0);
        assert_eq!(
            select_unique_recovery_match(std::slice::from_ref(&ended_near), &query, &|_| true)
                .map(|s| s.id.as_str()),
            Some("ended"),
            "ended_at near approx must match even if started_at is outside the skew window"
        );

        let nearer = recovery_summary("near", cwd, t0, None);
        let farther = recovery_summary("farther", cwd, t0 + chrono::Duration::seconds(90), None);
        assert_eq!(
            select_unique_recovery_match(&[nearer, farther], &query, &|_| true)
                .map(|s| s.id.as_str()),
            Some("near"),
            "a second candidate 90s away is unique enough to keep the nearer one"
        );
    }

    #[test]
    fn recovery_excludes_internal_then_ranks() {
        let t0 = Utc::now();
        let cwd = "/tmp/app";
        let internal = recovery_summary("internal", cwd, t0, None);
        let legal = recovery_summary("legal", cwd, t0 + chrono::Duration::seconds(90), None);
        let query = recovery_query(cwd, t0);
        assert_eq!(
            select_unique_recovery_match(&[internal, legal], &query, &|s| s.id != "internal")
                .map(|s| s.id.as_str()),
            Some("legal"),
            "internal nearest candidate must be dropped before ranking so the legal second wins"
        );
    }
}
