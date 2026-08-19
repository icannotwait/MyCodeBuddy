//! Codex Goal-cycle observer and native-rollout authority gate.
//!
//! Capability-qualified connections watch idle `session_info_update`
//! Goal / thread-status transitions and emit `BackgroundActivity` only after
//! matching rollout records are consumed. Goal cards stay on the existing
//! typed path; ACP `session/load` is never overlay or retirement authority.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use walkdir::WalkDir;

use crate::acp::autonomous_activity::{
    cap_normalized_turn_payload, complete_file_watermark, read_complete_record_batch,
    rotation_decision, AutonomousActivityPolicy, EpisodeRotation, ProviderRecordIdentities,
    TranscriptFileIdentity, EPISODE_RECORD_FORCE_ROTATE,
};
use crate::acp::grok_autonomous::Ownership;
use crate::acp::session_state::background_keepalive_max_age;
use crate::acp::types::BackgroundSettledInfo;
use crate::models::agent::AgentType;
use crate::models::message::{AutonomousTurnOrigin, MessageTurn};
use crate::parsers::codex::{
    codex_complete_records, codex_goal_turn_id, codex_record_payload,
    is_codex_goal_internal_context_message, parse_codex_goal_segment, resolve_codex_home_dir,
    rollout_session_id,
};

/// Last drained `BackgroundActivity` payload (tests + connection flush).
#[derive(Debug, Clone, Default)]
pub(crate) struct CodexEmitted {
    pub turns: Vec<MessageTurn>,
    pub outstanding: u32,
    pub settled: Vec<BackgroundSettledInfo>,
    pub watermark: u64,
}

/// How the adapter classified a raw dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexDispatchClaim {
    Unclaimed,
    AutonomousContent,
}

impl CodexDispatchClaim {
    pub(crate) fn skip_streaming_reducer(self) -> bool {
        matches!(self, Self::AutonomousContent)
    }
}

const AUTHORITY_RETRY: Duration = Duration::from_secs(30);
const TOMBSTONE_CAP: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authority {
    Provisional,
    Armed,
    Unsupported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EpisodePhase {
    Dormant,
    Opening,
    AwaitingAuthority,
    Open,
    AwaitingPersistedTerminal,
    Closed,
    SuppressedForeground,
    Abandoned,
}

struct Episode {
    phase: EpisodePhase,
    published_id: Option<String>,
    native_turn_id: Option<String>,
    opened_at: Instant,
    tail_from: u64,
    segment_from: u64,
    segment_record_count: usize,
    segment_part: u32,
}

impl Episode {
    fn dormant() -> Self {
        Self {
            phase: EpisodePhase::Dormant,
            published_id: None,
            native_turn_id: None,
            opened_at: Instant::now(),
            tail_from: 0,
            segment_from: 0,
            segment_record_count: 0,
            segment_part: 0,
        }
    }

    fn is_active(&self) -> bool {
        matches!(
            self.phase,
            EpisodePhase::Opening
                | EpisodePhase::AwaitingAuthority
                | EpisodePhase::Open
                | EpisodePhase::AwaitingPersistedTerminal
        )
    }
}

struct Tombstone {
    turn_id: String,
    at: Instant,
}

enum Discover {
    NotYetCreated,
    Armed(PathBuf),
    Ambiguous,
}

enum ThreadKind {
    Absent,
    Active,
    Idle,
    Unrecognized,
}

pub(crate) struct CodexAutonomousAdapter {
    session_id: String,
    sessions_root: Option<PathBuf>,
    rollout_path: Option<PathBuf>,
    authority: Authority,
    authority_deadline: Option<Instant>,
    committed: u64,
    goal_active: bool,
    episode: Episode,
    tombstones: VecDeque<Tombstone>,
    emitted: Option<CodexEmitted>,
    needs_detail_refetch: bool,
    file_identity: Option<TranscriptFileIdentity>,
    provider_record_identities: ProviderRecordIdentities,
}

impl CodexAutonomousAdapter {
    pub(crate) fn new() -> Self {
        Self {
            session_id: String::new(),
            sessions_root: None,
            rollout_path: None,
            authority: Authority::Provisional,
            authority_deadline: None,
            committed: 0,
            goal_active: false,
            episode: Episode::dormant(),
            tombstones: VecDeque::new(),
            emitted: None,
            needs_detail_refetch: false,
            file_identity: None,
            provider_record_identities: ProviderRecordIdentities::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(session_id: impl Into<String>, sessions_root: PathBuf) -> Self {
        let mut adapter = Self::new();
        adapter.session_id = session_id.into();
        adapter.sessions_root = Some(sessions_root);
        adapter
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn on_session_ready(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
        match self.discover_rollout() {
            Discover::Armed(path) => self.adopt_path(path, true),
            Discover::Ambiguous => self.downgrade_unsupported(),
            Discover::NotYetCreated => {
                self.authority = Authority::Provisional;
                self.committed = 0;
            }
        }
    }

    pub(crate) fn on_foreground_started(&mut self) {
        if matches!(
            self.episode.phase,
            EpisodePhase::Opening | EpisodePhase::AwaitingAuthority
        ) {
            self.episode.phase = EpisodePhase::SuppressedForeground;
        }
    }

    pub(crate) fn on_foreground_ended(&mut self) {
        if self.episode.phase == EpisodePhase::SuppressedForeground {
            self.episode = Episode::dormant();
        }
    }

    pub(crate) fn on_raw_dispatch(
        &mut self,
        method: &str,
        params: &Value,
        ownership: Ownership,
    ) -> CodexDispatchClaim {
        self.expire();
        if self.authority == Authority::Unsupported {
            if let Some(update) = session_info_update(method, params) {
                self.observe_session_info(update, params, ownership);
            }
            return CodexDispatchClaim::Unclaimed;
        }

        if let Some(update) = session_info_update(method, params) {
            self.observe_session_info(update, params, ownership);
            return CodexDispatchClaim::Unclaimed;
        }

        if !matches!(method, "session/update" | "_codex/session/update") {
            return CodexDispatchClaim::Unclaimed;
        }
        let Some(update) = params.get("update") else {
            return CodexDispatchClaim::Unclaimed;
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        if matches!(
            kind,
            "agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "tool_call_update"
        ) && ownership == Ownership::Idle
            && self.episode.is_active()
        {
            self.tail_once();
            return CodexDispatchClaim::AutonomousContent;
        }
        CodexDispatchClaim::Unclaimed
    }

    /// ACP `session/load` replay is bootstrap display data only.
    pub(crate) fn on_session_load_replay(&mut self, _items: &Value) {}

    pub(crate) fn on_disconnect(&mut self) {
        self.episode = Episode::dormant();
        self.tombstones.clear();
        self.emitted = None;
        self.needs_detail_refetch = false;
        self.authority_deadline = None;
        self.file_identity = None;
        self.provider_record_identities.clear();
    }

    pub(crate) fn tail_once(&mut self) {
        self.expire();
        if self.authority == Authority::Unsupported {
            return;
        }
        if self.episode.is_active() {
            if let Some(path) = self.rollout_path.clone() {
                let changed = TranscriptFileIdentity::for_path(&path)
                    .ok()
                    .zip(std::fs::metadata(&path).ok())
                    .is_some_and(|(identity, metadata)| {
                        self.file_identity
                            .as_ref()
                            .is_some_and(|known| known != &identity)
                            || metadata.len() < self.committed
                    });
                if changed {
                    self.reset_transcript_generation(&path);
                    return;
                }
            }
        }
        if self.episode.is_active() {
            self.resolve_authority();
        }
        if self.authority == Authority::Unsupported || !self.episode.is_active() {
            return;
        }
        let Some(path) = self.rollout_path.clone() else {
            return;
        };
        let Ok(identity) = TranscriptFileIdentity::for_path(&path) else {
            return;
        };
        let Ok(file_len) = std::fs::metadata(&path).map(|metadata| metadata.len()) else {
            return;
        };
        if self
            .file_identity
            .as_ref()
            .is_some_and(|known| known != &identity)
            || file_len < self.committed
        {
            self.reset_transcript_generation(&path);
            return;
        }
        self.file_identity.get_or_insert(identity);

        if self.episode.native_turn_id.is_none() {
            let Ok(batch) = read_complete_record_batch(&path, self.episode.tail_from) else {
                return;
            };
            self.remember_records(&batch.record_starts);
            if batch.skipped_oversized_record {
                self.committed = batch.next_offset;
                self.episode.tail_from = batch.next_offset;
                self.needs_detail_refetch = true;
                return;
            }
            if let Some((turn_id, proof_end)) =
                find_goal_native_turn_id(&batch.bytes, self.episode.tail_from, &self.tombstones)
            {
                if is_item_n_id(&turn_id) {
                    return;
                }
                self.episode.native_turn_id = Some(turn_id.clone());
                self.episode.published_id = Some(codex_goal_turn_id(&turn_id));
                self.episode.segment_from = proof_end;
                if matches!(
                    self.episode.phase,
                    EpisodePhase::Opening | EpisodePhase::AwaitingAuthority
                ) {
                    self.episode.phase = EpisodePhase::Open;
                }
            } else {
                let next_from = if batch.record_starts.len() >= EPISODE_RECORD_FORCE_ROTATE {
                    batch
                        .record_starts
                        .last()
                        .copied()
                        .unwrap_or(batch.next_offset)
                } else {
                    batch.next_offset
                };
                self.committed = next_from;
                self.episode.tail_from = next_from;
                return;
            }
        }

        let Some(native_turn_id) = self.episode.native_turn_id.clone() else {
            return;
        };
        let base_id = codex_goal_turn_id(&native_turn_id);
        let expected_id = segmented_turn_id(&base_id, self.episode.segment_part);
        if is_item_n_id(&expected_id) {
            return;
        }
        let Ok(batch) = read_complete_record_batch(&path, self.episode.segment_from) else {
            return;
        };
        self.remember_records(&batch.record_starts);
        if batch.skipped_oversized_record {
            self.committed = batch.next_offset;
            self.rotate_segment(batch.next_offset);
            self.needs_detail_refetch = true;
            return;
        }
        self.episode.segment_record_count = batch.record_starts.len();
        let terminal_persisted =
            has_task_complete(&batch.bytes, &native_turn_id, self.episode.segment_from);
        let Some(mut turn) =
            parse_codex_goal_segment(&batch.bytes, &self.session_id, &native_turn_id)
        else {
            self.committed = batch.next_offset;
            if self.episode.phase == EpisodePhase::AwaitingPersistedTerminal && terminal_persisted {
                self.finish_episode();
            }
            return;
        };
        turn.id = expected_id.clone();
        if turn.blocks.is_empty()
            || turn.autonomous_origin != Some(AutonomousTurnOrigin::AgentAutonomous)
        {
            return;
        }
        self.committed = batch.next_offset;
        self.episode.published_id = Some(expected_id);
        if let Some(turn) = cap_normalized_turn_payload(turn) {
            self.emit_turn(turn);
        }

        if rotation_decision(self.episode.segment_record_count, terminal_persisted)
            == Some(EpisodeRotation::Forced)
            && !terminal_persisted
        {
            self.rotate_segment(batch.next_offset);
            return;
        }

        if self.episode.phase == EpisodePhase::AwaitingPersistedTerminal && terminal_persisted {
            self.finish_episode();
        }
    }

    pub(crate) fn take_emitted(&mut self) -> CodexEmitted {
        self.take_activity().unwrap_or_else(|| CodexEmitted {
            turns: Vec::new(),
            outstanding: self.outstanding(),
            settled: Vec::new(),
            watermark: self.committed,
        })
    }

    pub(crate) fn take_activity(&mut self) -> Option<CodexEmitted> {
        self.emitted.take()
    }

    pub(crate) fn take_detail_refetch(&mut self) -> bool {
        let pending = self.needs_detail_refetch;
        self.needs_detail_refetch = false;
        pending
    }

    pub(crate) fn outstanding(&self) -> u32 {
        if self.goal_active || self.episode.is_active() {
            1
        } else {
            0
        }
    }

    pub(crate) fn autonomous_busy(&self) -> bool {
        self.authority != Authority::Unsupported && self.episode.is_active()
    }

    pub(crate) fn needs_periodic_tail(&self) -> bool {
        self.autonomous_busy()
    }

    pub(crate) fn is_unsupported(&self) -> bool {
        matches!(self.authority, Authority::Unsupported)
    }

    #[cfg(test)]
    pub(crate) fn expire_authority_window_now(&mut self) {
        self.authority_deadline = Some(Instant::now() - Duration::from_secs(1));
    }

    fn observe_session_info(&mut self, update: &Value, params: &Value, ownership: Ownership) {
        let Some(meta) = update.get("_meta").or_else(|| params.get("_meta")) else {
            return;
        };
        if let Some(active) = goal_is_active(meta) {
            let was = self.goal_active;
            self.goal_active = active;
            if was != self.goal_active {
                self.emit_accounting();
            }
        }
        match thread_kind(meta) {
            ThreadKind::Absent => {}
            ThreadKind::Unrecognized => self.downgrade_unsupported(),
            ThreadKind::Active => self.on_thread_active(ownership),
            ThreadKind::Idle => self.on_thread_idle(ownership),
        }
    }

    fn on_thread_active(&mut self, ownership: Ownership) {
        if self.authority == Authority::Unsupported {
            return;
        }
        if ownership != Ownership::Idle {
            if matches!(
                self.episode.phase,
                EpisodePhase::Opening | EpisodePhase::AwaitingAuthority
            ) {
                self.episode.phase = EpisodePhase::SuppressedForeground;
            }
            return;
        }
        if !self.goal_active {
            return;
        }
        if self.episode.is_active() {
            self.tail_once();
            return;
        }
        self.episode = Episode {
            phase: EpisodePhase::Opening,
            published_id: None,
            native_turn_id: None,
            opened_at: Instant::now(),
            tail_from: self.committed,
            segment_from: self.committed,
            segment_record_count: 0,
            segment_part: 0,
        };
        if self.authority_deadline.is_none() && self.authority == Authority::Provisional {
            self.authority_deadline = Some(Instant::now() + AUTHORITY_RETRY);
        }
        self.resolve_authority();
        self.emit_accounting();
    }

    fn on_thread_idle(&mut self, ownership: Ownership) {
        if ownership != Ownership::Idle || !self.episode.is_active() {
            return;
        }
        self.episode.phase = EpisodePhase::AwaitingPersistedTerminal;
        self.tail_once();
    }

    fn resolve_authority(&mut self) {
        if self.authority == Authority::Unsupported {
            return;
        }
        if self.authority == Authority::Armed {
            if let Some(path) = &self.rollout_path {
                if is_regular_file(path) {
                    match rollout_session_id(path).as_deref() {
                        Some(id) if id == self.session_id => return,
                        Some(_) => {
                            self.downgrade_unsupported();
                            return;
                        }
                        None => {
                            self.authority = Authority::Provisional;
                            self.rollout_path = None;
                            self.file_identity = None;
                        }
                    }
                }
            }
            self.authority = Authority::Provisional;
            self.rollout_path = None;
        }
        if self.authority_window_expired() {
            self.downgrade_unsupported();
            return;
        }
        match self.discover_rollout() {
            Discover::Armed(path) => {
                self.adopt_path(path, false);
                if self.episode.phase == EpisodePhase::AwaitingAuthority {
                    self.episode.phase = EpisodePhase::Opening;
                }
            }
            Discover::Ambiguous => self.downgrade_unsupported(),
            Discover::NotYetCreated => {
                if self.episode.phase == EpisodePhase::Opening {
                    self.episode.phase = EpisodePhase::AwaitingAuthority;
                }
            }
        }
        if self.authority_window_expired() && self.authority != Authority::Armed {
            self.downgrade_unsupported();
        }
    }

    fn authority_window_expired(&self) -> bool {
        self.authority != Authority::Armed
            && self
                .authority_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn adopt_path(&mut self, path: PathBuf, rebaseline: bool) {
        self.rollout_path = Some(path.clone());
        self.authority = Authority::Armed;
        self.file_identity = TranscriptFileIdentity::for_path(&path).ok();
        if rebaseline {
            if let Ok(watermark) = complete_file_watermark(&path) {
                self.committed = watermark;
            }
        }
    }

    fn discover_rollout(&self) -> Discover {
        if self.session_id.is_empty() {
            return Discover::NotYetCreated;
        }
        let root = self
            .sessions_root
            .clone()
            .unwrap_or_else(|| resolve_codex_home_dir().join("sessions"));
        if !root.exists() {
            return Discover::NotYetCreated;
        }
        let mut matches = Vec::new();
        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !is_regular_file(path) {
                continue;
            }
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            if !fname.starts_with("rollout-") {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if rollout_session_id(path).as_deref() == Some(self.session_id.as_str()) {
                matches.push(path.to_path_buf());
            }
        }
        match matches.len() {
            1 => Discover::Armed(matches.remove(0)),
            n if n > 1 => Discover::Ambiguous,
            _ => Discover::NotYetCreated,
        }
    }

    fn downgrade_unsupported(&mut self) {
        self.authority = Authority::Unsupported;
        self.rollout_path = None;
        if self.episode.is_active() {
            self.episode.phase = EpisodePhase::Closed;
        }
        self.authority_deadline = None;
        self.emit_accounting();
    }

    fn finish_episode(&mut self) {
        if let Some(turn_id) = self
            .episode
            .native_turn_id
            .clone()
            .or_else(|| self.episode.published_id.clone())
        {
            self.tombstones.push_back(Tombstone {
                turn_id,
                at: Instant::now(),
            });
            while self.tombstones.len() > TOMBSTONE_CAP {
                self.tombstones.pop_front();
            }
        }
        self.episode.phase = EpisodePhase::Closed;
        self.needs_detail_refetch = true;
        self.emit_accounting();
    }

    fn emit_accounting(&mut self) {
        let outstanding = self.outstanding();
        let watermark = self.committed;
        if let Some(emitted) = &mut self.emitted {
            emitted.outstanding = outstanding;
            emitted.settled.clear();
            emitted.watermark = watermark;
            return;
        }
        self.emitted = Some(CodexEmitted {
            turns: Vec::new(),
            outstanding,
            settled: Vec::new(),
            watermark,
        });
    }

    fn emit_turn(&mut self, turn: MessageTurn) {
        if turn.autonomous_origin != Some(AutonomousTurnOrigin::AgentAutonomous) {
            return;
        }
        if is_item_n_id(&turn.id) {
            return;
        }
        self.emitted = Some(CodexEmitted {
            turns: vec![turn],
            outstanding: self.outstanding(),
            settled: Vec::new(),
            watermark: self.committed,
        });
    }

    fn expire(&mut self) {
        let max_age = keepalive_std();
        self.tombstones.retain(|t| t.at.elapsed() < max_age);
        if self.episode.is_active() && self.episode.opened_at.elapsed() >= max_age {
            if self.episode.phase == EpisodePhase::AwaitingPersistedTerminal {
                self.needs_detail_refetch = true;
            }
            self.episode.phase = EpisodePhase::Abandoned;
        }
    }

    fn remember_records(&mut self, starts: &[u64]) {
        for start in starts {
            self.provider_record_identities
                .remember(format!("{}:{start}", self.episode.segment_part));
        }
    }

    fn rotate_segment(&mut self, next_offset: u64) {
        self.episode.segment_from = next_offset;
        self.episode.tail_from = next_offset;
        self.episode.segment_record_count = 0;
        self.episode.segment_part = self.episode.segment_part.saturating_add(1);
        self.episode.published_id = None;
    }

    fn reset_transcript_generation(&mut self, path: &Path) {
        self.episode = Episode::dormant();
        self.tombstones.clear();
        self.provider_record_identities.clear();
        self.committed = complete_file_watermark(path).unwrap_or(0);
        self.needs_detail_refetch = true;
        self.authority = Authority::Provisional;
        self.authority_deadline = None;
        self.rollout_path = None;
        self.file_identity = None;
        self.emit_accounting();
    }
}

/// Prompt-receive gate used by `connection.rs`. One flag with Grok — do not
/// invent a second prompt channel.
pub(crate) fn should_hold_prompt(adapter: Option<&CodexAutonomousAdapter>) -> bool {
    adapter.is_some_and(|adapter| adapter.autonomous_busy())
}

pub(crate) fn adapter_for_connection(
    agent: AgentType,
    hidden_generation: bool,
    caps: &crate::acp::autonomous_activity::AutonomousCapabilities,
) -> Option<CodexAutonomousAdapter> {
    if hidden_generation {
        return None;
    }
    match AutonomousActivityPolicy::for_connection(agent, caps) {
        AutonomousActivityPolicy::CodexGoalTranscript => Some(CodexAutonomousAdapter::new()),
        _ => None,
    }
}

fn session_info_update<'a>(method: &str, params: &'a Value) -> Option<&'a Value> {
    if !matches!(method, "session/update" | "_codex/session/update") {
        return None;
    }
    let update = params.get("update")?;
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("session_info_update") {
        return None;
    }
    Some(update)
}

fn goal_is_active(meta: &Value) -> Option<bool> {
    let goal = meta.get("goal")?;
    if goal.is_null() {
        return Some(false);
    }
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("active")
        .trim()
        .to_ascii_lowercase();
    Some(status == "active")
}

fn thread_kind(meta: &Value) -> ThreadKind {
    let Some(codex) = meta.get("codex") else {
        return ThreadKind::Absent;
    };
    let Some(status) = codex.get("threadStatus") else {
        return ThreadKind::Absent;
    };
    let ty = status
        .as_str()
        .or_else(|| status.get("type").and_then(Value::as_str));
    match ty.map(str::trim).map(|s| s.to_ascii_lowercase()) {
        Some(ty) if ty == "active" => ThreadKind::Active,
        Some(ty) if ty == "idle" => ThreadKind::Idle,
        Some(_) => ThreadKind::Unrecognized,
        None => ThreadKind::Unrecognized,
    }
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

fn is_item_n_id(id: &str) -> bool {
    id.strip_prefix("item-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn record_turn_id(payload: &Value) -> Option<String> {
    payload
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn extract_user_input_text(payload: &Value) -> Option<String> {
    let content = payload.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("input_text") {
            continue;
        }
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    }
}

fn find_goal_native_turn_id(
    bytes: &[u8],
    base_offset: u64,
    tombstones: &VecDeque<Tombstone>,
) -> Option<(String, u64)> {
    let mut pending: Option<String> = None;
    let mut goal_owned = false;
    for (relative_start, record) in codex_complete_records(bytes) {
        let start = base_offset.saturating_add(relative_start);
        let Ok(line) = std::str::from_utf8(codex_record_payload(record)) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "event_msg" => {
                let payload = value.get("payload");
                let ptype = payload
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match ptype {
                    "task_started" => {
                        pending = payload.and_then(record_turn_id);
                        goal_owned = false;
                    }
                    "user_message" => {
                        if payload
                            .and_then(|p| p.get("message"))
                            .and_then(Value::as_str)
                            .is_some_and(is_codex_goal_internal_context_message)
                        {
                            goal_owned = true;
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let payload = value.get("payload");
                let is_user_message = payload.and_then(|p| p.get("type")).and_then(Value::as_str)
                    == Some("message")
                    && payload.and_then(|p| p.get("role")).and_then(Value::as_str) == Some("user");
                if is_user_message
                    && payload
                        .and_then(extract_user_input_text)
                        .as_deref()
                        .is_some_and(is_codex_goal_internal_context_message)
                {
                    goal_owned = true;
                }
            }
            _ => {}
        }
        if let (Some(turn_id), true) = (pending.as_deref(), goal_owned) {
            if !is_item_n_id(turn_id) && !tombstones.iter().any(|tomb| tomb.turn_id == turn_id) {
                return Some((turn_id.to_string(), start + record.len() as u64));
            }
        }
    }
    None
}

fn has_task_complete(bytes: &[u8], turn_id: &str, from: u64) -> bool {
    for (relative_start, record) in codex_complete_records(bytes) {
        let _start = from.saturating_add(relative_start);
        let Ok(line) = std::str::from_utf8(codex_record_payload(record)) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("task_complete") {
            continue;
        }
        if record_turn_id(payload).as_deref() == Some(turn_id) {
            return true;
        }
    }
    false
}

fn segmented_turn_id(base: &str, part: u32) -> String {
    if part == 0 {
        base.to_string()
    } else {
        format!("{base}:part:{part}")
    }
}

fn keepalive_std() -> Duration {
    background_keepalive_max_age()
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(3600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::autonomous_activity::{
        normalized_turn_payload_len, AutonomousCapabilities, EPISODE_PAYLOAD_MAX_BYTES,
        PROVIDER_RECORD_IDENTITY_CAP,
    };
    use crate::parsers::codex::{codex_goal_turn_id, parse_codex_rollout};
    use serde_json::json;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn qualified_caps() -> AutonomousCapabilities {
        AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        }
    }

    fn tmp_sessions() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        (dir, root)
    }

    fn write_rollout(sessions_root: &Path, payload_id: &str, body: &str) -> PathBuf {
        let dir = sessions_root.join("2026").join("08").join("18");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-08-18T00-00-00-{payload_id}.jsonl"));
        fs::write(&path, body).unwrap();
        path
    }

    fn append_line(path: &Path, line: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
        if !line.ends_with('\n') {
            f.write_all(b"\n").unwrap();
        }
    }

    fn session_meta_line(id: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-18T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/tmp"}}}}"#
        )
    }

    fn task_started_line(turn_id: &str, ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_started","turn_id":"{turn_id}"}}}}"#
        )
    }

    fn task_complete_line(turn_id: &str, ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"{turn_id}"}}}}"#
        )
    }

    fn goal_context_line(ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"user","id":"msg_hidden","content":[{{"type":"input_text","text":"<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n</codex_internal_context>"}}]}}}}"#
        )
    }

    fn assistant_line(id: &str, text: &str, ts: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"assistant","id":"{id}","content":[{{"type":"output_text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn pathological_episode_force_rotates_and_keeps_identity_and_payload_bounds() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");

        let mut episode = String::new();
        episode.push_str(&task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"));
        episode.push('\n');
        episode.push_str(&goal_context_line("2026-08-18T00:00:02Z"));
        episode.push('\n');
        for index in 0..(EPISODE_RECORD_FORCE_ROTATE + 1) {
            episode.push_str(&assistant_line(
                &format!("msg_{index}"),
                &format!("chunk-{index};"),
                "2026-08-18T00:00:03Z",
            ));
            episode.push('\n');
        }
        append_line(&path, episode.trim_end());

        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        let first = adapter.take_emitted();
        assert_eq!(first.turns.len(), 1);
        assert!(normalized_turn_payload_len(&first.turns[0]) <= EPISODE_PAYLOAD_MAX_BYTES);
        assert!(adapter.provider_record_identities.len() <= PROVIDER_RECORD_IDENTITY_CAP);
        assert!(adapter.episode.segment_record_count < EPISODE_RECORD_FORCE_ROTATE);
        let first_id = first.turns[0].id.clone();

        adapter.tail_once();
        let second = adapter.take_emitted();
        assert_eq!(second.turns.len(), 1);
        assert_ne!(second.turns[0].id, first_id);
        assert!(format!("{:?}", second.turns[0].blocks).contains("chunk-1024"));
    }

    #[test]
    fn terminal_only_segment_after_force_rotation_closes_episode() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        let mut episode = format!(
            "{}\n{}\n",
            task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
            goal_context_line("2026-08-18T00:00:02Z")
        );
        for index in 0..EPISODE_RECORD_FORCE_ROTATE {
            episode.push_str(&assistant_line(
                &format!("msg_{index}"),
                &format!("chunk-{index};"),
                "2026-08-18T00:00:03Z",
            ));
            episode.push('\n');
        }
        append_line(&path, episode.trim_end());
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        assert_eq!(adapter.episode.segment_record_count, 0);

        append_line(
            &path,
            &task_complete_line("turn_goal_1", "2026-08-18T00:00:04Z"),
        );
        feed_thread(&mut adapter, "idle", Ownership::Idle);
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_detail_refetch());
    }

    #[test]
    fn replacement_discards_episode_refetches_and_revokes_native_authority() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        assert!(adapter.autonomous_busy());
        assert_eq!(adapter.take_emitted().turns.len(), 1);

        let replacement = path.with_extension("replacement");
        fs::write(
            &replacement,
            format!("{}\n", session_meta_line("other-session")),
        )
        .unwrap();
        fs::remove_file(&path).unwrap();
        fs::rename(&replacement, &path).unwrap();

        adapter.tail_once();
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_detail_refetch());
        assert_eq!(adapter.authority, Authority::Provisional);
        assert!(adapter.rollout_path.is_none());
        assert!(adapter.episode.native_turn_id.is_none());
        assert!(adapter.episode.published_id.is_none());
        assert_eq!(adapter.provider_record_identities.len(), 0);

        feed_thread(&mut adapter, "active", Ownership::Idle);
        assert_ne!(adapter.authority, Authority::Armed);
        assert!(adapter.episode.native_turn_id.is_none());
    }

    fn session_info_params(goal_status: Option<&str>, thread_type: Option<&str>) -> Value {
        let mut meta = serde_json::Map::new();
        if let Some(status) = goal_status {
            meta.insert(
                "goal".into(),
                json!({ "status": status, "objective": "ship it" }),
            );
        }
        if let Some(thread) = thread_type {
            meta.insert(
                "codex".into(),
                json!({ "threadStatus": { "type": thread } }),
            );
        }
        json!({
            "sessionId": "sess",
            "update": {
                "sessionUpdate": "session_info_update",
                "_meta": meta
            }
        })
    }

    fn feed_goal(adapter: &mut CodexAutonomousAdapter, status: &str, ownership: Ownership) {
        adapter.on_raw_dispatch(
            "session/update",
            &session_info_params(Some(status), None),
            ownership,
        );
    }

    fn feed_thread(adapter: &mut CodexAutonomousAdapter, thread: &str, ownership: Ownership) {
        adapter.on_raw_dispatch(
            "session/update",
            &session_info_params(None, Some(thread)),
            ownership,
        );
    }

    #[test]
    fn adapter_requires_codex_goal_transcript_policy() {
        assert!(adapter_for_connection(AgentType::Codex, false, &qualified_caps()).is_some());
        assert!(adapter_for_connection(
            AgentType::Codex,
            false,
            &AutonomousCapabilities::default()
        )
        .is_none());
        assert!(adapter_for_connection(AgentType::Grok, false, &qualified_caps()).is_none());
        assert!(adapter_for_connection(AgentType::Codex, true, &qualified_caps()).is_none());
    }

    #[test]
    fn goal_active_alone_does_not_open_episode() {
        let (_dir, root) = tmp_sessions();
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        feed_goal(&mut adapter, "active", Ownership::Idle);
        assert!(
            !adapter.autonomous_busy(),
            "Goal active alone must not open an episode"
        );
        assert!(adapter.take_emitted().turns.is_empty());
        assert_eq!(
            adapter.outstanding(),
            1,
            "active Goal still contributes one keepalive unit"
        );
        assert!(!should_hold_prompt(Some(&adapter)));
    }

    #[test]
    fn idle_thread_active_under_goal_opens_after_rollout_proves_turn() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        let emitted = adapter.take_emitted();
        assert_eq!(emitted.turns.len(), 1);
        assert_eq!(emitted.turns[0].id, codex_goal_turn_id("turn_goal_1"));
        assert!(emitted.settled.is_empty());
        assert!(emitted.watermark > 0);
    }

    #[test]
    fn foreground_thread_active_does_not_overlay() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "no overlay", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Foreground);
        adapter.tail_once();
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_emitted().turns.is_empty());
    }

    #[test]
    fn goal_complete_does_not_close_episode() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        assert!(adapter.autonomous_busy(), "episode must be open");
        let _ = adapter.take_emitted();

        feed_goal(&mut adapter, "complete", Ownership::Idle);
        append_line(
            &path,
            &assistant_line("msg_more", "still going", "2026-08-18T00:00:04Z"),
        );
        adapter.tail_once();
        assert!(
            adapter.autonomous_busy(),
            "Goal complete must not close the episode"
        );
        let more = adapter.take_emitted();
        assert_eq!(more.turns.len(), 1);
        assert_eq!(more.turns[0].id, codex_goal_turn_id("turn_goal_1"));

        append_line(
            &path,
            &task_complete_line("turn_goal_1", "2026-08-18T00:00:05Z"),
        );
        feed_thread(&mut adapter, "idle", Ownership::Idle);
        adapter.tail_once();
        assert!(
            !adapter.autonomous_busy(),
            "idle threadStatus closes after persisted task_complete"
        );
    }

    #[test]
    fn two_idle_cycles_get_distinct_native_ids() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");

        append_line(&path, &task_started_line("turn_a", "2026-08-18T00:00:01Z"));
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_a", "first", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        let first_id = adapter.take_emitted().turns[0].id.clone();
        append_line(&path, &task_complete_line("turn_a", "2026-08-18T00:00:04Z"));
        feed_thread(&mut adapter, "idle", Ownership::Idle);
        adapter.tail_once();
        assert!(!adapter.autonomous_busy());

        append_line(&path, &task_started_line("turn_b", "2026-08-18T00:00:05Z"));
        append_line(&path, &goal_context_line("2026-08-18T00:00:06Z"));
        append_line(
            &path,
            &assistant_line("msg_b", "second", "2026-08-18T00:00:07Z"),
        );
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.tail_once();
        let second = adapter.take_emitted();
        assert_eq!(second.turns.len(), 1);
        assert_eq!(first_id, codex_goal_turn_id("turn_a"));
        assert_eq!(second.turns[0].id, codex_goal_turn_id("turn_b"));
        assert_ne!(first_id, second.turns[0].id);
    }

    #[test]
    fn item_n_ids_are_not_canonical() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.on_session_load_replay(&json!({
            "sessionId": "sess-1",
            "items": [{ "id": "item-1", "type": "message" }]
        }));
        adapter.tail_once();
        let emitted = adapter.take_emitted();
        assert_eq!(emitted.turns.len(), 1);
        assert_eq!(emitted.turns[0].id, codex_goal_turn_id("turn_goal_1"));
        assert_ne!(emitted.turns[0].id, "item-1");
    }

    #[test]
    fn missing_rollout_after_30s_downgrades_only_autonomous() {
        let (_dir, root) = tmp_sessions();
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-missing", root);
        adapter.on_session_ready("sess-missing");
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        adapter.expire_authority_window_now();
        adapter.tail_once();
        assert!(
            adapter.is_unsupported(),
            "timeout must downgrade autonomous handling only"
        );
        assert!(adapter.take_emitted().turns.is_empty());
        assert!(!adapter.autonomous_busy());
        assert!(!should_hold_prompt(Some(&adapter)));
        assert_eq!(
            adapter.outstanding(),
            1,
            "Goal cards + keepalive continue after autonomous downgrade"
        );
    }

    #[test]
    fn other_session_rollout_stays_provisional_until_matching_file_appears() {
        let (_dir, root) = tmp_sessions();
        write_rollout(
            &root,
            "other",
            &format!("{}\n", session_meta_line("other-session")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root.clone());
        adapter.on_session_ready("sess-1");
        assert!(
            !adapter.is_unsupported(),
            "other sessions must not downgrade before this session's file exists"
        );

        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        assert!(
            adapter.autonomous_busy(),
            "idle open stays busy and not Unsupported while this rollout is absent"
        );
        assert!(!adapter.is_unsupported());
        assert!(adapter.take_emitted().turns.is_empty());

        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        adapter.tail_once();
        let emitted = adapter.take_emitted();
        assert_eq!(
            emitted.turns.len(),
            1,
            "later matching rollout must recover"
        );
        assert_eq!(emitted.turns[0].id, codex_goal_turn_id("turn_goal_1"));
        assert!(emitted.watermark > 0);
    }

    #[test]
    fn mismatched_session_id_is_unsupported() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        feed_goal(&mut adapter, "active", Ownership::Idle);
        feed_thread(&mut adapter, "active", Ownership::Idle);
        assert!(!adapter.is_unsupported());

        std::fs::write(&path, format!("{}\n", session_meta_line("other-session"))).unwrap();
        adapter.tail_once();
        assert!(
            adapter.is_unsupported(),
            "adopted path whose session_meta.id became another session is a mismatch"
        );
        assert!(!adapter.autonomous_busy());
    }

    #[test]
    fn prompt_gate_and_keepalive_unit() {
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(
            &root,
            "sess-1",
            &format!("{}\n", session_meta_line("sess-1")),
        );
        let mut adapter = CodexAutonomousAdapter::new_for_test("sess-1", root);
        adapter.on_session_ready("sess-1");
        feed_goal(&mut adapter, "active", Ownership::Idle);
        assert_eq!(adapter.outstanding(), 1, "outstanding 1 while goal active");
        assert!(!adapter.autonomous_busy());
        assert!(!should_hold_prompt(Some(&adapter)));
        assert!(!should_hold_prompt(None));

        append_line(
            &path,
            &task_started_line("turn_goal_1", "2026-08-18T00:00:01Z"),
        );
        append_line(&path, &goal_context_line("2026-08-18T00:00:02Z"));
        append_line(
            &path,
            &assistant_line("msg_live", "working", "2026-08-18T00:00:03Z"),
        );
        feed_thread(&mut adapter, "active", Ownership::Idle);
        assert!(adapter.autonomous_busy());
        assert!(should_hold_prompt(Some(&adapter)));
        assert_eq!(adapter.outstanding(), 1);

        append_line(
            &path,
            &task_complete_line("turn_goal_1", "2026-08-18T00:00:04Z"),
        );
        feed_thread(&mut adapter, "idle", Ownership::Idle);
        adapter.tail_once();
        assert!(!adapter.autonomous_busy());
        assert!(!should_hold_prompt(Some(&adapter)));
        assert_eq!(
            adapter.outstanding(),
            1,
            "Goal still active after the cycle closes"
        );

        feed_goal(&mut adapter, "complete", Ownership::Idle);
        assert_eq!(
            adapter.outstanding(),
            0,
            "0 only when Goal is non-active and no episode is open"
        );
    }

    const CODEX_TWO_CYCLE: &str = include_str!("fixtures/codex_goal_autonomous_two_cycles.jsonl");

    fn codex_fixture_lines() -> Vec<&'static str> {
        CODEX_TWO_CYCLE
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    fn fixture_session_id(lines: &[&str]) -> String {
        for line in lines {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            if let Some(id) = value
                .pointer("/payload/id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                return id.to_string();
            }
        }
        panic!("fixture must start with session_meta.payload.id");
    }

    fn payload_turn_id(line: &str) -> Option<String> {
        serde_json::from_str::<Value>(line)
            .ok()?
            .pointer("/payload/turn_id")?
            .as_str()
            .map(str::to_string)
    }

    fn is_event_type(line: &str, payload_type: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some(payload_type)
    }

    fn assistant_output_text(line: &str) -> Option<String> {
        let value = serde_json::from_str::<Value>(line).ok()?;
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            return None;
        }
        let payload = value.get("payload")?;
        if payload.get("type").and_then(Value::as_str) != Some("message")
            || payload.get("role").and_then(Value::as_str) != Some("assistant")
        {
            return None;
        }
        extract_codex_output_text_for_fixture(payload)
    }

    fn extract_codex_output_text_for_fixture(payload: &Value) -> Option<String> {
        let content = payload.get("content")?.as_array()?;
        let mut parts = Vec::new();
        for item in content {
            if item.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(""))
        }
    }

    fn split_codex_two_cycle_fixture(
        lines: &[&str],
    ) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        let mut preamble = Vec::new();
        let mut cycle1 = Vec::new();
        let mut cycle2_head = Vec::new();
        let mut cycle2_tail = Vec::new();
        let mut stage: u8 = 0;
        for line in lines {
            if is_event_type(line, "task_started") {
                match payload_turn_id(line).as_deref() {
                    Some("turn_goal_1") => stage = 1,
                    Some("turn_goal_2") => stage = 2,
                    _ => {}
                }
            }
            if stage == 2
                && assistant_output_text(line)
                    .is_some_and(|text| text.contains("after goal complete"))
            {
                stage = 3;
            }
            let owned = (*line).to_string();
            match stage {
                0 => preamble.push(owned),
                1 => cycle1.push(owned),
                2 => cycle2_head.push(owned),
                _ => cycle2_tail.push(owned),
            }
        }
        (preamble, cycle1, cycle2_head, cycle2_tail)
    }

    fn write_lines(path: &Path, lines: &[String]) {
        for line in lines {
            append_line(path, line);
        }
    }

    fn blocks_contain_internal_context(blocks: &[crate::models::message::ContentBlock]) -> bool {
        use crate::models::message::ContentBlock;
        blocks.iter().any(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
                text.contains("codex_internal_context") || text.contains("Continue working toward")
            }
            ContentBlock::ToolUse { input_preview, .. } => input_preview
                .as_deref()
                .is_some_and(|text| text.contains("codex_internal_context")),
            ContentBlock::ToolResult { output_preview, .. } => output_preview
                .as_deref()
                .is_some_and(|text| text.contains("codex_internal_context")),
            _ => false,
        })
    }

    fn take_autonomous_turn(adapter: &mut CodexAutonomousAdapter) -> (MessageTurn, u64) {
        let emitted = adapter.take_emitted();
        assert!(emitted.settled.is_empty());
        assert_eq!(emitted.turns.len(), 1);
        assert_eq!(
            emitted.turns[0].autonomous_origin,
            Some(AutonomousTurnOrigin::AgentAutonomous)
        );
        assert!(
            !emitted.turns[0].id.starts_with("item-"),
            "item-N ids are not canonical"
        );
        assert!(
            !blocks_contain_internal_context(&emitted.turns[0].blocks),
            "internal context never renders"
        );
        assert!(emitted.watermark > 0, "do not emit an unwatermarked turn");
        let watermark = emitted.watermark;
        (emitted.turns.into_iter().next().unwrap(), watermark)
    }

    fn idle_content_hint(adapter: &mut CodexAutonomousAdapter) {
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"agent_message_chunk",
                "content":{"type":"text","text":"redacted live hint"}
            }}),
            Ownership::Idle,
        );
    }

    #[test]
    fn codex_two_cycle_fixture_keeps_native_ids_after_replay() {
        let lines = codex_fixture_lines();
        assert!(
            !lines.is_empty(),
            "codex_goal_autonomous_two_cycles.jsonl must contain the two-cycle rollout"
        );
        let (preamble, cycle1, cycle2_head, cycle2_tail) = split_codex_two_cycle_fixture(&lines);
        assert!(
            !preamble.is_empty() && !cycle1.is_empty() && !cycle2_head.is_empty(),
            "fixture must include a foreground terminal plus two Goal cycles"
        );

        let session_id = fixture_session_id(&lines);
        let (_dir, root) = tmp_sessions();
        let path = write_rollout(&root, &session_id, &format!("{}\n", preamble.join("\n")));
        assert_eq!(
            rollout_session_id(&path).as_deref(),
            Some(session_id.as_str())
        );

        let mut adapter = CodexAutonomousAdapter::new_for_test(&session_id, root);
        adapter.on_session_ready(&session_id);

        feed_goal(&mut adapter, "active", Ownership::Idle);
        assert!(
            !adapter.autonomous_busy(),
            "Goal active alone must not open an episode"
        );
        feed_thread(&mut adapter, "active", Ownership::Idle);
        assert!(adapter.autonomous_busy());

        write_lines(&path, &cycle1);
        idle_content_hint(&mut adapter);
        adapter.tail_once();
        let (first, first_watermark) = take_autonomous_turn(&mut adapter);
        assert_eq!(first.id, codex_goal_turn_id("turn_goal_1"));
        let mut last_overlay_watermark = first_watermark;

        feed_thread(&mut adapter, "idle", Ownership::Idle);
        adapter.tail_once();
        assert!(
            !adapter.autonomous_busy(),
            "first cycle closes on idle + task_complete"
        );

        feed_thread(&mut adapter, "active", Ownership::Idle);
        feed_goal(&mut adapter, "complete", Ownership::Idle);
        assert!(
            adapter.autonomous_busy(),
            "Goal complete must not truncate/close the second cycle"
        );

        write_lines(&path, &cycle2_head);
        idle_content_hint(&mut adapter);
        adapter.tail_once();
        let (second_head, head_watermark) = take_autonomous_turn(&mut adapter);
        assert_eq!(second_head.id, codex_goal_turn_id("turn_goal_2"));
        assert_ne!(first.id, second_head.id);
        last_overlay_watermark = last_overlay_watermark.max(head_watermark);

        write_lines(&path, &cycle2_tail);
        idle_content_hint(&mut adapter);
        adapter.tail_once();
        let (second, second_watermark) = take_autonomous_turn(&mut adapter);
        assert_eq!(second.id, second_head.id);
        let second_text = format!("{:?}", second.blocks);
        assert!(
            second_text.contains("after goal complete"),
            "Goal complete must not truncate the second turn: {second_text}"
        );
        last_overlay_watermark = last_overlay_watermark.max(second_watermark);

        feed_thread(&mut adapter, "idle", Ownership::Idle);
        adapter.tail_once();
        adapter.on_session_load_replay(&json!({
            "sessionId": session_id,
            "items": [
                {"id": "item-1", "type": "message"},
                {"id": "item-2", "type": "message"},
                {"id": "item-3", "type": "message"}
            ]
        }));
        adapter.tail_once();
        let after_replay = adapter.take_emitted();
        if let Some(turn) = after_replay.turns.first() {
            assert_eq!(turn.id, second.id);
            assert!(!turn.id.starts_with("item-"));
        }
        last_overlay_watermark = last_overlay_watermark.max(after_replay.watermark);

        let (turns, parser_watermark) =
            parse_codex_rollout(&path, &session_id).expect("cold parse rollout");
        assert!(
            parser_watermark >= last_overlay_watermark,
            "parser watermark {parser_watermark} must cover last overlay watermark {last_overlay_watermark}"
        );
        let autos: Vec<&MessageTurn> = turns
            .iter()
            .filter(|turn| turn.autonomous_origin == Some(AutonomousTurnOrigin::AgentAutonomous))
            .collect();
        assert_eq!(autos.len(), 2, "two independent agent_autonomous turns");
        assert_eq!(autos[0].id, first.id);
        assert_eq!(autos[1].id, second.id);
        assert!(autos.iter().all(|turn| !turn.id.starts_with("item-")));
        assert!(turns
            .iter()
            .all(|turn| !blocks_contain_internal_context(&turn.blocks)));
    }
}
