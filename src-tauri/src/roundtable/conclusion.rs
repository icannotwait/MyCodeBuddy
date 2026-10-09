//! One-click export of an accepted roundtable conclusion.
//!
//! Only the latest *published* moderator synthesis of a *completed* room is
//! rendered. Speaker aliases (`s0`, `s1`, ...) become seat names, and the
//! document is redacted before it leaves the server (download, copy or a
//! workspace file). Saving writes below the registered workspace folder only:
//! the root comes from the `folder` row, never from the client, and every path
//! component is opened with `O_NOFOLLOW` relative to the previous directory.
//!
//! These two commands deliberately live outside the sealed 18-command
//! protocol set: they read already-published state and never mutate the room.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use regex::Regex;
use roundtable_protocol::{ActorContext, ErrorCode, RoomId, RoundtableConfigV1, RtResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::store::{column, num, optional_row, text, RoundtableStore};
use super::{rt_error, RoundtableService};

pub(crate) const CONCLUSION_COMMANDS: [&str; 2] =
    ["roundtable_conclusion_export", "roundtable_conclusion_save"];

const EXPORT_DIR: [&str; 2] = ["docs", "roundtable"];
const MAX_SAVE_AS: u32 = 99;
const MIN_SECRET_LEN: usize = 12;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    room_id: RoomId,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SaveMode {
    Create,
    Overwrite,
    SaveAs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    room_id: RoomId,
    request_id: uuid::Uuid,
    mode: SaveMode,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SaveRecord {
    request_id: String,
    mode: SaveMode,
    relative_path: String,
    sha256: String,
    bytes: u64,
    status: String,
    saved_at: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SaveLedger {
    saves: Vec<SaveRecord>,
}

pub(crate) struct Rendered {
    pub markdown: String,
    pub file_name: String,
    pub redactions: usize,
    message_id: String,
    body_hash: String,
    workspace: Option<PathBuf>,
}

impl RoundtableService {
    pub(crate) async fn execute_conclusion(
        self: &Arc<Self>,
        actor: &ActorContext,
        command: &str,
        body: Value,
    ) -> RtResult<Value> {
        if !body.is_object() || body.get("principal_id").is_some() {
            return Err(rt_error(ErrorCode::InvalidArgument, "principal"));
        }
        let store = self.command_store()?;
        match command {
            "roundtable_conclusion_export" => {
                let request: ExportRequest = decode(body)?;
                let rendered = render_room(
                    &store,
                    actor,
                    &request.room_id,
                    request.locale.as_deref(),
                    &self.data_dir,
                )
                .await?;
                let saves = read_ledger(&self.data_dir, &request.room_id)?.saves;
                Ok(json!({
                    "room_id": request.room_id,
                    "message_id": rendered.message_id,
                    "body_hash": rendered.body_hash,
                    "file_name": rendered.file_name,
                    "relative_path": format!("{}/{}", EXPORT_DIR.join("/"), rendered.file_name),
                    "markdown": rendered.markdown,
                    "redactions": rendered.redactions,
                    "workspace_available": rendered.workspace.is_some(),
                    "saves": saves,
                }))
            }
            "roundtable_conclusion_save" => {
                let request: SaveRequest = decode(body)?;
                // One writer at a time keeps the ledger and name probing atomic.
                static SAVE_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
                let _gate = SAVE_GATE
                    .get_or_init(|| tokio::sync::Mutex::new(()))
                    .lock()
                    .await;
                let rendered = render_room(
                    &store,
                    actor,
                    &request.room_id,
                    request.locale.as_deref(),
                    &self.data_dir,
                )
                .await?;
                let mut ledger = read_ledger(&self.data_dir, &request.room_id)?;
                let request_key = request.request_id.to_string();
                if let Some(previous) = ledger.saves.iter().find(|s| s.request_id == request_key) {
                    if previous.mode != request.mode {
                        return Err(rt_error(ErrorCode::IdempotencyConflict, "request_id"));
                    }
                    let mut replay =
                        serde_json::to_value(previous).map_err(|_| storage("ledger"))?;
                    replay["replayed"] = json!(true);
                    return Ok(replay);
                }
                let root = rendered
                    .workspace
                    .clone()
                    .ok_or_else(|| rt_error(ErrorCode::Forbidden, "workspace_not_found"))?;
                let (relative_path, status) = write_conclusion(
                    &root,
                    &rendered.file_name,
                    rendered.markdown.as_bytes(),
                    request.mode,
                )?;
                let record = SaveRecord {
                    request_id: request_key,
                    mode: request.mode,
                    relative_path,
                    sha256: format!("{:x}", Sha256::digest(rendered.markdown.as_bytes())),
                    bytes: rendered.markdown.len() as u64,
                    status: status.into(),
                    saved_at: chrono::Local::now().to_rfc3339(),
                };
                ledger.saves.push(record.clone());
                write_ledger(&self.data_dir, &request.room_id, &ledger)?;
                let mut out = serde_json::to_value(&record).map_err(|_| storage("ledger"))?;
                out["replayed"] = json!(false);
                Ok(out)
            }
            _ => Err(rt_error(ErrorCode::InvalidArgument, "command")),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: Value) -> RtResult<T> {
    serde_json::from_value(body).map_err(|_| rt_error(ErrorCode::InvalidArgument, "request"))
}

fn storage(reason: &'static str) -> roundtable_protocol::RtError {
    rt_error(ErrorCode::StorageUnavailable, reason)
}

async fn render_room(
    store: &RoundtableStore,
    actor: &ActorContext,
    room: &RoomId,
    locale: Option<&str>,
    data_dir: &Path,
) -> RtResult<Rendered> {
    let connection = store.connection();
    let row = optional_row(
        connection,
        "SELECT config_ref,status FROM rt_rooms WHERE room_id=? AND principal_id=?",
        vec![
            text(&room.to_string()),
            text(&actor.principal_id().to_string()),
        ],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::Forbidden, "not_found"))?;
    let config: RoundtableConfigV1 =
        serde_json::from_str(&column::<String>(&row, 0)?).map_err(|_| storage("room_config"))?;
    if column::<String>(&row, 1)? != "completed" {
        return Err(rt_error(ErrorCode::InvalidState, "conclusion_unavailable"));
    }
    // Latest membership per message decides visibility; only the moderator's
    // published synthesis is the accepted conclusion.
    let message = optional_row(
        connection,
        "SELECT m.message_id,m.body_json,m.body_hash FROM rt_messages m \
         JOIN rt_speakers s ON s.room_id=m.room_id AND s.speaker_id=m.speaker_id \
         JOIN rt_message_memberships mm ON mm.room_id=m.room_id AND mm.message_id=m.message_id \
         WHERE m.room_id=? AND s.role='moderator' AND mm.visibility='published' \
         AND mm.membership_version=(SELECT MAX(x.membership_version) FROM rt_message_memberships x \
             WHERE x.room_id=m.room_id AND x.message_id=m.message_id) \
         ORDER BY mm.published_seq DESC LIMIT 1",
        vec![text(&room.to_string())],
    )
    .await?
    .ok_or_else(|| rt_error(ErrorCode::InvalidState, "conclusion_unavailable"))?;
    let message_id: String = column(&message, 0)?;
    let body: Value =
        serde_json::from_str(&column::<String>(&message, 1)?).map_err(|_| storage("synthesis"))?;
    if body["kind"] != "synthesis" {
        return Err(rt_error(ErrorCode::InvalidState, "conclusion_unavailable"));
    }
    let body_hash: String = column(&message, 2)?;
    let mut models = BTreeMap::new();
    for speaker in super::store::rows(
        connection,
        "SELECT ordinal,model_id FROM rt_speakers WHERE room_id=? AND role='member'",
        vec![text(&room.to_string())],
    )
    .await?
    {
        models.insert(column::<i64>(&speaker, 0)?, column::<String>(&speaker, 1)?);
    }
    let workspace = match config.workspace_id.parse::<i64>().ok().filter(|id| *id > 0) {
        Some(id) => optional_row(
            connection,
            "SELECT path FROM folder WHERE id=? AND deleted_at IS NULL AND kind='regular'",
            vec![num(id)],
        )
        .await?
        .map(|row| column::<String>(&row, 0).map(PathBuf::from))
        .transpose()?
        .filter(|path| path.is_absolute()),
        None => None,
    };
    let seats = config
        .participants
        .iter()
        .map(|p| Seat {
            ordinal: p.ordinal,
            agent: agent_name(p.agent.as_deref()),
            role: p.role.clone(),
            model: p
                .model
                .clone()
                .or_else(|| models.get(&i64::from(p.ordinal)).cloned()),
            moderator: p.ordinal == config.moderator_ordinal,
        })
        .collect::<Vec<_>>();
    let now = chrono::Local::now();
    let doc = Document {
        room_id: room.to_string(),
        topic: config.topic.clone(),
        display_name: config.display_name.clone(),
        seats,
        body: &body,
        message_id: message_id.clone(),
        body_hash: body_hash.clone(),
        generated_at: now.format("%Y-%m-%d %H:%M %Z").to_string(),
        zh: locale.is_some_and(|l| l.starts_with("zh")),
    };
    let (markdown, redactions) = redact(&render(&doc), &secret_values(data_dir));
    let title = config.display_name.as_deref().unwrap_or(&config.topic);
    Ok(Rendered {
        markdown,
        file_name: file_name(
            &now.format("%Y-%m-%d").to_string(),
            title,
            &room.to_string(),
        ),
        redactions,
        message_id,
        body_hash,
        workspace,
    })
}

fn agent_name(agent: Option<&str>) -> String {
    match agent.unwrap_or("codex") {
        "grok" => "Grok".into(),
        "antigravity" => "Antigravity".into(),
        "cursor" => "Cursor".into(),
        "codex" => "Codex".into(),
        other => other.to_string(),
    }
}

pub(crate) struct Seat {
    pub ordinal: u32,
    pub agent: String,
    pub role: String,
    pub model: Option<String>,
    pub moderator: bool,
}

pub(crate) struct Document<'a> {
    pub room_id: String,
    pub topic: String,
    pub display_name: Option<String>,
    pub seats: Vec<Seat>,
    pub body: &'a Value,
    pub message_id: String,
    pub body_hash: String,
    pub generated_at: String,
    pub zh: bool,
}

struct Labels {
    title: &'static str,
    question: &'static str,
    participants: &'static str,
    moderator: &'static str,
    recommendation: &'static str,
    consensus: &'static str,
    supported_by: &'static str,
    disagreements: &'static str,
    risks: &'static str,
    decisions: &'static str,
    alternatives: &'static str,
    inference: &'static str,
    none: &'static str,
}

const EN: Labels = Labels {
    title: "Roundtable conclusion",
    question: "Question",
    participants: "Participants",
    moderator: "moderator",
    recommendation: "Recommendation",
    consensus: "Consensus",
    supported_by: "supported by",
    disagreements: "Disagreements",
    risks: "Risks",
    decisions: "Decisions needed",
    alternatives: "Alternatives considered",
    inference: "inference",
    none: "None.",
};

const ZH: Labels = Labels {
    title: "圆桌结论",
    question: "议题",
    participants: "参与者",
    moderator: "主持人",
    recommendation: "建议",
    consensus: "共识",
    supported_by: "支持者",
    disagreements: "分歧",
    risks: "风险",
    decisions: "待决策事项",
    alternatives: "备选方案",
    inference: "推断",
    none: "无。",
};

fn seat_label(seat: &Seat) -> String {
    if seat.role.trim().is_empty() {
        seat.agent.clone()
    } else {
        format!("{} ({})", seat.agent, seat.role.trim())
    }
}

fn alias_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bs(\d{1,3})\b").expect("alias regex"))
}

/// Replace speaker aliases with seat names. Unknown ordinals stay untouched so
/// unrelated words such as "s3 bucket" are never rewritten into a wrong seat.
fn substitute_aliases(input: &str, seats: &[Seat]) -> String {
    alias_regex()
        .replace_all(input, |caps: &regex::Captures<'_>| {
            caps[1]
                .parse::<u32>()
                .ok()
                .and_then(|n| seats.iter().find(|s| s.ordinal == n))
                .map(seat_label)
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

pub(crate) fn render(doc: &Document<'_>) -> String {
    let l = if doc.zh { &ZH } else { &EN };
    let names = |value: &str| substitute_aliases(value, &doc.seats);
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("roundtable_room: {}\n", yaml_string(&doc.room_id)));
    out.push_str(&format!(
        "synthesis_message: {}\n",
        yaml_string(&doc.message_id)
    ));
    out.push_str(&format!(
        "synthesis_hash: {}\n",
        yaml_string(&doc.body_hash)
    ));
    out.push_str(&format!(
        "generated_at: {}\n",
        yaml_string(&doc.generated_at)
    ));
    out.push_str("participants:\n");
    for seat in &doc.seats {
        out.push_str(&format!(
            "  - {{ seat: {}, agent: {}, role: {}, model: {}{} }}\n",
            yaml_string(&format!("s{}", seat.ordinal)),
            yaml_string(&seat.agent),
            yaml_string(&seat.role),
            yaml_string(seat.model.as_deref().unwrap_or("")),
            if seat.moderator {
                ", moderator: true"
            } else {
                ""
            },
        ));
    }
    let refs = topic_references(&doc.topic);
    if !refs.is_empty() {
        out.push_str("references:\n");
        for reference in refs {
            out.push_str(&format!("  - {}\n", yaml_string(&reference)));
        }
    }
    out.push_str("---\n\n");
    let heading = doc
        .display_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("{}: {}", l.title, name.trim()))
        .unwrap_or_else(|| l.title.to_string());
    out.push_str(&format!("# {heading}\n\n## {}\n\n", l.question));
    for line in doc.topic.trim().lines() {
        out.push_str(&format!("> {line}\n"));
    }
    out.push_str(&format!("\n## {}\n\n", l.participants));
    for seat in &doc.seats {
        out.push_str(&format!("- {}", seat_label(seat)));
        if let Some(model) = seat.model.as_deref().filter(|m| !m.is_empty()) {
            out.push_str(&format!(" · `{model}`"));
        }
        if seat.moderator {
            out.push_str(&format!(" · {}", l.moderator));
        }
        out.push('\n');
    }
    let marker = |item: &Value| {
        if item["inference"] == true {
            format!(" _({})_", l.inference)
        } else {
            String::new()
        }
    };
    out.push_str(&format!("\n## {}\n\n", l.recommendation));
    let recommendation = &doc.body["recommendation"];
    out.push_str(&format!(
        "{}{}\n",
        names(recommendation["text"].as_str().unwrap_or("").trim()),
        marker(recommendation)
    ));
    out.push_str(&format!("\n## {}\n\n", l.consensus));
    let consensus = doc.body["consensus_items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if consensus.is_empty() {
        out.push_str(&format!("{}\n", l.none));
    }
    for item in &consensus {
        out.push_str(&format!(
            "- {}{}",
            names(item["text"].as_str().unwrap_or("").trim()),
            marker(item)
        ));
        let supporters = item["supporter_aliases"]
            .as_array()
            .map(|aliases| {
                aliases
                    .iter()
                    .filter_map(Value::as_str)
                    .map(names)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !supporters.is_empty() {
            out.push_str(&format!(" — {}: {}", l.supported_by, supporters.join(", ")));
        }
        out.push('\n');
    }
    for (key, title) in [
        ("disagreements", l.disagreements),
        ("risks", l.risks),
        ("decision_requests", l.decisions),
        ("alternatives", l.alternatives),
    ] {
        out.push_str(&format!("\n## {title}\n\n"));
        let items = doc.body[key].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            out.push_str(&format!("{}\n", l.none));
        }
        for item in &items {
            out.push_str(&format!(
                "- {}{}\n",
                names(item["text"].as_str().unwrap_or("").trim()),
                marker(item)
            ));
        }
    }
    out
}

/// `@relative/path` references written in the topic (see the topic composer).
pub(crate) fn topic_references(topic: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:^|\s)@([^\s@]+)").expect("ref regex"));
    let mut refs = Vec::new();
    for caps in re.captures_iter(topic) {
        let path = caps[1].trim_end_matches([',', '.', ';', ':', ')', '，', '。']);
        if !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|s| s == "..") {
            let path = path.to_string();
            if !refs.contains(&path) {
                refs.push(path);
            }
        }
    }
    refs
}

pub(crate) fn file_name(date: &str, title: &str, room: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.chars().count() >= 48 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "roundtable" } else { slug };
    let room8: String = room
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    format!("{date}-{slug}-{room8}.md")
}

/// Every credential value the server can see: host-held and mounted auth
/// files of installed adapters, provider credential env vars, and any
/// secret-looking environment variable. Missing files only skip that source;
/// pattern redaction still applies.
fn secret_values(data_dir: &Path) -> Vec<String> {
    let mut secrets = Vec::new();
    if let Ok(catalog) = super::installed_runtime::InstalledRuntime::load_catalog(data_dir) {
        for runtime in catalog.values() {
            for mount in runtime
                .oci
                .host_held_credentials
                .iter()
                .chain(runtime.oci.auth_mounts.iter())
            {
                if let Ok(values) = super::diagnostics::auth_file_secrets(&mount.source) {
                    secrets.extend(values);
                }
            }
            for provider in &runtime.providers {
                if let Ok(value) = std::env::var(&provider.credential_env) {
                    secrets.push(value);
                }
            }
        }
    }
    let name = Regex::new(r"(?i)(TOKEN|SECRET|PASSWORD|PASSWD|API_?KEY|PRIVATE_?KEY|CREDENTIAL)")
        .expect("env name regex");
    for (key, value) in std::env::vars() {
        if name.is_match(&key) {
            secrets.push(value);
        }
    }
    secrets
}

const REDACTED: &str = "[REDACTED]";

/// Exact known secrets first (longest first), then common credential shapes.
pub(crate) fn redact(input: &str, secrets: &[String]) -> (String, usize) {
    let mut secrets = secrets
        .iter()
        .map(|s| s.trim())
        .filter(|s| s.chars().count() >= MIN_SECRET_LEN)
        .map(str::to_string)
        .collect::<Vec<_>>();
    secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    secrets.dedup();
    let mut output = input.to_string();
    let mut count = 0;
    for secret in &secrets {
        let hits = output.matches(secret.as_str()).count();
        if hits > 0 {
            count += hits;
            output = output.replace(secret.as_str(), REDACTED);
        }
    }
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----", REDACTED),
            (r"\b(?:sk|xai|sk-ant|sk-proj)-[A-Za-z0-9_\-]{16,}", REDACTED),
            (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}", REDACTED),
            (r"\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
            (r"\bglpat-[A-Za-z0-9_\-]{20,}", REDACTED),
            (r"\bxox[abposr]-[A-Za-z0-9\-]{10,}", REDACTED),
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
            (r"\bAIza[0-9A-Za-z_\-]{35}\b", REDACTED),
            (r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}", REDACTED),
            (r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=\-]{16,}", "${1}[REDACTED]"),
            (
                r#"(?i)\b((?:api[_-]?key|access[_-]?token|auth[_-]?token|secret|password|passwd|client[_-]?secret)\s*[:=]\s*["']?)[^\s"'`]{8,}"#,
                "${1}[REDACTED]",
            ),
            (r"(?i)\b([a-z][a-z0-9+.\-]*://[^/\s:@]+:)[^@\s/]{4,}@", "${1}[REDACTED]@"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).expect("redaction regex"), replacement))
        .collect()
    });
    for (pattern, replacement) in patterns {
        let hits = pattern.find_iter(&output).count();
        if hits > 0 {
            count += hits;
            output = pattern.replace_all(&output, *replacement).into_owned();
        }
    }
    (output, count)
}

fn ledger_path(data_dir: &Path, room: &RoomId) -> PathBuf {
    data_dir
        .join("roundtable/exports")
        .join(format!("{room}.json"))
}

fn read_ledger(data_dir: &Path, room: &RoomId) -> RtResult<SaveLedger> {
    match std::fs::read(ledger_path(data_dir, room)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| storage("export_ledger")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SaveLedger::default()),
        Err(_) => Err(storage("export_ledger")),
    }
}

fn write_ledger(data_dir: &Path, room: &RoomId, ledger: &SaveLedger) -> RtResult<()> {
    let path = ledger_path(data_dir, room);
    let parent = path.parent().ok_or_else(|| storage("export_ledger"))?;
    std::fs::create_dir_all(parent).map_err(|_| storage("export_ledger"))?;
    let temp = parent.join(format!(".{room}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec_pretty(ledger).map_err(|_| storage("export_ledger"))?;
    std::fs::write(&temp, bytes).map_err(|_| storage("export_ledger"))?;
    std::fs::rename(&temp, &path).map_err(|_| {
        let _ = std::fs::remove_file(&temp);
        storage("export_ledger")
    })
}

/// Returns the workspace-relative path written and `created` / `overwritten`.
#[cfg(unix)]
pub(crate) fn write_conclusion(
    root: &Path,
    file_name: &str,
    bytes: &[u8],
    mode: SaveMode,
) -> RtResult<(String, &'static str)> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    if file_name.contains('/') || file_name.starts_with('.') || !file_name.ends_with(".md") {
        return Err(rt_error(ErrorCode::InvalidArgument, "file_name"));
    }
    let cstr = |value: &str| {
        std::ffi::CString::new(value).map_err(|_| rt_error(ErrorCode::InvalidArgument, "path"))
    };
    let refused = || rt_error(ErrorCode::Forbidden, "workspace_path_refused");
    // The registered root is canonicalized once; everything below it is
    // walked with O_NOFOLLOW so a planted symlink cannot redirect the write.
    let root = std::fs::canonicalize(root).map_err(|_| refused())?;
    let open_dir = |parent: Option<&OwnedFd>, name: &str| -> RtResult<OwnedFd> {
        let path = cstr(name)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: valid NUL-terminated path; the fd is owned immediately.
        let fd = unsafe {
            match parent {
                Some(dir) => libc::openat(dir.as_raw_fd(), path.as_ptr(), flags),
                None => libc::open(path.as_ptr(), flags),
            }
        };
        if fd < 0 {
            return Err(refused());
        }
        // SAFETY: fd was just returned by open/openat and is not shared.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    };
    let root_text = root.to_str().ok_or_else(refused)?;
    let mut dir = open_dir(None, root_text)?;
    for component in EXPORT_DIR {
        let name = cstr(component)?;
        // SAFETY: dir is a live directory fd; name is NUL-terminated.
        let made = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) };
        if made != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(refused());
        }
        dir = open_dir(Some(&dir), component)?;
    }
    let exists = |name: &str| -> RtResult<Option<bool>> {
        let path = cstr(name)?;
        // SAFETY: stat buffer is zeroed and owned; fd and path are valid.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                path.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Ok(None);
        }
        Ok(Some((stat.st_mode & libc::S_IFMT) == libc::S_IFREG))
    };
    let create = |name: &str| -> RtResult<Option<std::fs::File>> {
        let path = cstr(name)?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: dir is a live directory fd; the returned fd is owned below.
        let fd =
            unsafe { libc::openat(dir.as_raw_fd(), path.as_ptr(), flags, 0o644 as libc::c_uint) };
        if fd < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                return Ok(None);
            }
            return Err(refused());
        }
        // SAFETY: fd was just created and is exclusively owned.
        Ok(Some(unsafe { std::fs::File::from_raw_fd(fd) }))
    };
    let write_all = |mut file: std::fs::File| -> RtResult<()> {
        file.write_all(bytes)
            .map_err(|_| storage("conclusion_write"))?;
        file.sync_all().map_err(|_| storage("conclusion_write"))
    };
    let relative = |name: &str| format!("{}/{name}", EXPORT_DIR.join("/"));
    match mode {
        SaveMode::Create => match create(file_name)? {
            Some(file) => {
                write_all(file)?;
                Ok((relative(file_name), "created"))
            }
            None => Err(rt_error(ErrorCode::RevisionConflict, "already_exists")),
        },
        SaveMode::SaveAs => {
            let stem = file_name.trim_end_matches(".md");
            for n in 2..=MAX_SAVE_AS {
                let candidate = format!("{stem}-{n}.md");
                if let Some(file) = create(&candidate)? {
                    write_all(file)?;
                    return Ok((relative(&candidate), "created"));
                }
            }
            Err(rt_error(ErrorCode::RevisionConflict, "already_exists"))
        }
        SaveMode::Overwrite => {
            if exists(file_name)? == Some(false) {
                // Never replace a symlink, directory or device.
                return Err(refused());
            }
            let temp = format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4().simple());
            let file = create(&temp)?.ok_or_else(refused)?;
            let temp_c = cstr(&temp)?;
            if let Err(error) = write_all(file) {
                // SAFETY: removes only the temp entry this call created.
                unsafe { libc::unlinkat(dir.as_raw_fd(), temp_c.as_ptr(), 0) };
                return Err(error);
            }
            let target = cstr(file_name)?;
            // SAFETY: both names are relative to the same verified directory fd.
            let renamed = unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    temp_c.as_ptr(),
                    dir.as_raw_fd(),
                    target.as_ptr(),
                )
            };
            if renamed != 0 {
                unsafe { libc::unlinkat(dir.as_raw_fd(), temp_c.as_ptr(), 0) };
                return Err(storage("conclusion_write"));
            }
            Ok((relative(file_name), "overwritten"))
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn write_conclusion(
    _root: &Path,
    _file_name: &str,
    _bytes: &[u8],
    _mode: SaveMode,
) -> RtResult<(String, &'static str)> {
    Err(rt_error(
        ErrorCode::PolicyUnenforceable,
        "workspace_write_unsupported",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seats() -> Vec<Seat> {
        vec![
            Seat {
                ordinal: 0,
                agent: "Grok".into(),
                role: "Proposer".into(),
                model: Some("grok-4.6".into()),
                moderator: true,
            },
            Seat {
                ordinal: 1,
                agent: "Antigravity".into(),
                role: "Critic".into(),
                model: None,
                moderator: false,
            },
        ]
    }

    fn body() -> Value {
        json!({
            "kind":"synthesis",
            "recommendation":{"text":"Adopt cookies; s1 agreed with s0.","aliases":[],"inference":false},
            "consensus_items":[{"text":"HttpOnly blocks XSS theft","agreement_level":"explicit","aliases":[],"inference":false,"supporter_aliases":["s0","s1"],"support_response_aliases":[]}],
            "disagreements":[{"text":"s1 doubts SameSite=Lax; store in an s3 bucket? s9","aliases":[],"inference":true}],
            "risks":[],"decision_requests":[],"alternatives":[{"text":"localStorage","aliases":[],"inference":false}]
        })
    }

    fn doc(body: &Value, zh: bool) -> Document<'_> {
        Document {
            room_id: "f268e2bd-355f-4a2e-b70d-c4d8bc243e8a".into(),
            topic: "Review @src/auth.rs and @docs/plan.md.\nKeep it short".into(),
            display_name: None,
            seats: seats(),
            body,
            message_id: "m-1".into(),
            body_hash: "abc".into(),
            generated_at: "2026-10-09 10:00 PDT".into(),
            zh,
        }
    }

    #[test]
    fn renders_the_accepted_synthesis_with_seat_names() {
        let body = body();
        let markdown = render(&doc(&body, false));
        assert!(markdown.starts_with("---\nroundtable_room: \"f268e2bd"));
        assert!(
            markdown.contains("Adopt cookies; Antigravity (Critic) agreed with Grok (Proposer).")
        );
        assert!(markdown.contains("supported by: Grok (Proposer), Antigravity (Critic)"));
        assert!(markdown.contains(
            "Antigravity (Critic) doubts SameSite=Lax; store in an s3 bucket? s9 _(inference)_"
        ));
        assert!(markdown.contains("- Grok (Proposer) · `grok-4.6` · moderator"));
        assert!(markdown.contains("## Risks\n\nNone."));
        assert!(markdown.contains("references:\n  - \"src/auth.rs\"\n  - \"docs/plan.md\"\n"));
        assert!(markdown.contains("> Review @src/auth.rs and @docs/plan.md.\n> Keep it short\n"));
        let prose = markdown.split("## Question").nth(1).unwrap();
        assert!(!prose.contains("s0") && !prose.contains("s1 "));
        let zh = render(&doc(&body, true));
        assert!(zh.contains("# 圆桌结论") && zh.contains("## 共识") && zh.contains("支持者"));
    }

    #[test]
    fn topic_references_skip_absolute_and_parent_paths() {
        assert_eq!(
            topic_references("see @a/b.rs, @/etc/passwd @../x @a/b.rs mail@example.com @dir/"),
            vec!["a/b.rs".to_string(), "dir/".to_string()]
        );
    }

    #[test]
    fn file_names_are_dated_slugged_and_room_scoped() {
        assert_eq!(
            file_name(
                "2026-10-09",
                "Should we store tokens in HttpOnly cookies?",
                "f268e2bd-355f"
            ),
            "2026-10-09-should-we-store-tokens-in-httponly-cookies-f268e2bd.md"
        );
        assert_eq!(
            file_name("2026-10-09", "登录 令牌/存储", "ab12cd34ef"),
            "2026-10-09-登录-令牌-存储-ab12cd34.md"
        );
        assert_eq!(
            file_name("2026-10-09", "../../", "ab12cd34"),
            "2026-10-09-roundtable-ab12cd34.md"
        );
        let long = file_name("2026-10-09", &"x".repeat(500), "ab12cd34");
        assert!(long.len() < 80 && !long.contains('/'));
    }

    #[test]
    fn redacts_known_secrets_and_credential_shapes() {
        let known = "known-oauth-refresh-value-123456".to_string();
        let input = format!(
            "a {known} b sk-proj-ABCDEFGHIJKLMNOPQRSTUV c ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345 \
             Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123 AKIAABCDEFGHIJKLMNOP \
             password=hunter2hunter2 https://user:pa55word@example.com/x short-ok"
        );
        let (out, count) = redact(&input, &[known.clone(), "short".into()]);
        for leaked in [
            known.as_str(),
            "sk-proj-ABC",
            "ghp_ABC",
            "abcdefghijklmnopqrstuvwxyz0123",
            "AKIAABCDEFGHIJKLMNOP",
            "hunter2hunter2",
            "pa55word",
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert!(out.contains("Bearer [REDACTED]") && out.contains("short-ok"));
        assert!(count >= 7);
    }

    #[cfg(unix)]
    #[test]
    fn saves_inside_the_workspace_with_conflicts_and_no_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let name = "2026-10-09-topic-ab12cd34.md";
        let (path, status) = write_conclusion(root.path(), name, b"one", SaveMode::Create).unwrap();
        assert_eq!(
            (path.as_str(), status),
            ("docs/roundtable/2026-10-09-topic-ab12cd34.md", "created")
        );
        let error = write_conclusion(root.path(), name, b"two", SaveMode::Create).unwrap_err();
        assert_eq!(error.details.reason.as_deref(), Some("already_exists"));
        let (copy, _) = write_conclusion(root.path(), name, b"two", SaveMode::SaveAs).unwrap();
        assert_eq!(copy, "docs/roundtable/2026-10-09-topic-ab12cd34-2.md");
        let (same, status) =
            write_conclusion(root.path(), name, b"three", SaveMode::Overwrite).unwrap();
        assert_eq!((same.as_str(), status), (path.as_str(), "overwritten"));
        let dir = root.path().join("docs/roundtable");
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), b"three");
        assert_eq!(
            std::fs::read(dir.join("2026-10-09-topic-ab12cd34-2.md")).unwrap(),
            b"two"
        );
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));

        // A planted symlink as the target is never followed or replaced.
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.md");
        std::fs::write(&victim, b"keep").unwrap();
        symlink(&victim, dir.join("linked.md")).unwrap();
        assert!(write_conclusion(root.path(), "linked.md", b"x", SaveMode::Overwrite).is_err());
        assert!(write_conclusion(root.path(), "linked.md", b"x", SaveMode::Create).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");

        // A symlinked docs/ directory cannot redirect the write.
        let other = tempfile::tempdir().unwrap();
        symlink(outside.path(), other.path().join("docs")).unwrap();
        let error = write_conclusion(other.path(), name, b"x", SaveMode::Create).unwrap_err();
        assert_eq!(
            error.details.reason.as_deref(),
            Some("workspace_path_refused")
        );
        assert!(!outside.path().join("roundtable").exists());

        for bad in ["../x.md", ".hidden.md", "a/b.md", "x.txt"] {
            assert!(
                write_conclusion(root.path(), bad, b"x", SaveMode::Create).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn save_ledger_round_trips() {
        let data = tempfile::tempdir().unwrap();
        let room: RoomId = "f268e2bd-355f-4a2e-b70d-c4d8bc243e8a".parse().unwrap();
        assert!(read_ledger(data.path(), &room).unwrap().saves.is_empty());
        let ledger = SaveLedger {
            saves: vec![SaveRecord {
                request_id: "r".into(),
                mode: SaveMode::SaveAs,
                relative_path: "docs/roundtable/x.md".into(),
                sha256: "h".into(),
                bytes: 1,
                status: "created".into(),
                saved_at: "t".into(),
            }],
        };
        write_ledger(data.path(), &room, &ledger).unwrap();
        assert_eq!(read_ledger(data.path(), &room).unwrap().saves, ledger.saves);
    }
}
