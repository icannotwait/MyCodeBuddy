//! Grok idle-wire observer and `updates.jsonl` tailer.
//!
//! Watches raw ACP dispatches (including private `_x.ai/*` methods) and
//! persists a complete-byte tail of the Grok transcript so background-task
//! follow-ups surface as `AcpEvent::BackgroundActivity` without owning the
//! foreground prompt path.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::acp::autonomous_activity::{
    cap_normalized_turn_payload, complete_file_watermark, read_complete_record_batch,
    rotation_decision, AutonomousActivityPolicy, EpisodeRotation, ProviderRecordIdentities,
    TranscriptFileIdentity, EPISODE_PAYLOAD_MAX_BYTES, EPISODE_RECORD_FORCE_ROTATE,
};
use crate::acp::session_state::background_keepalive_max_age;
use crate::acp::types::BackgroundSettledInfo;
use crate::models::agent::AgentType;
use crate::models::message::{AutonomousTurnOrigin, MessageTurn};
use crate::parsers::grok::{
    grok_autonomous_turn_from_segment, grok_autonomous_turn_id, grok_complete_records,
    grok_record_payload, grok_reminder_task_ids, is_grok_background_task_reminder,
};

/// Connection-loop ownership of the dispatch currently being observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ownership {
    Idle,
    Foreground,
}

/// How the adapter classified a raw dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokDispatchClaim {
    Unclaimed,
    Accounting,
    AutonomousContent,
    IdleTerminal,
}

impl GrokDispatchClaim {
    pub(crate) fn skip_streaming_reducer(self) -> bool {
        matches!(self, Self::AutonomousContent | Self::IdleTerminal)
    }

    pub(crate) fn is_idle_terminal(self) -> bool {
        matches!(self, Self::IdleTerminal)
    }
}

/// Last drained `BackgroundActivity` payload (tests + connection flush).
#[derive(Debug, Clone, Default)]
pub(crate) struct GrokEmitted {
    pub turns: Vec<MessageTurn>,
    pub outstanding: u32,
    pub settled: Vec<BackgroundSettledInfo>,
    pub watermark: u64,
}

const TASK_CAP: usize = 64;
const TOMBSTONE_CAP: usize = 16;

struct TaskEntry {
    started_at: Instant,
}

struct SettledTask {
    id: String,
    at: Instant,
}

#[derive(Clone)]
struct Episode {
    phase: EpisodePhase,
    task_ids: Vec<String>,
    trigger_start: Option<u64>,
    published_id: Option<String>,
    opened_at: Instant,
    tail_from: u64,
    segment_from: u64,
    segment_record_count: usize,
    segment_part: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EpisodePhase {
    Dormant,
    Opening,
    Open,
    AwaitingPersistedTerminal,
    Closed,
    SuppressedForeground,
    Abandoned,
}

impl Episode {
    fn dormant() -> Self {
        Self {
            phase: EpisodePhase::Dormant,
            task_ids: Vec::new(),
            trigger_start: None,
            published_id: None,
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
            EpisodePhase::Opening | EpisodePhase::Open | EpisodePhase::AwaitingPersistedTerminal
        )
    }
}

struct Tombstone {
    trigger_start: u64,
    task_ids: Vec<String>,
    at: Instant,
}

pub(crate) struct GrokAutonomousAdapter {
    session_id: String,
    updates_path: Option<PathBuf>,
    committed: u64,
    baseline_ready: bool,
    tasks: HashMap<String, TaskEntry>,
    task_order: VecDeque<String>,
    recently_settled: VecDeque<SettledTask>,
    last_idle_was_task_completed: bool,
    last_visible_is_user: bool,
    episode: Episode,
    tombstones: VecDeque<Tombstone>,
    emitted: Option<GrokEmitted>,
    needs_detail_refetch: bool,
    last_visible_user_log: Option<Instant>,
    file_identity: Option<TranscriptFileIdentity>,
    provider_record_identities: ProviderRecordIdentities,
}

impl GrokAutonomousAdapter {
    pub(crate) fn new() -> Self {
        Self {
            session_id: String::new(),
            updates_path: None,
            committed: 0,
            baseline_ready: false,
            tasks: HashMap::new(),
            task_order: VecDeque::new(),
            recently_settled: VecDeque::new(),
            last_idle_was_task_completed: false,
            last_visible_is_user: false,
            episode: Episode::dormant(),
            tombstones: VecDeque::new(),
            emitted: None,
            needs_detail_refetch: false,
            last_visible_user_log: None,
            file_identity: None,
            provider_record_identities: ProviderRecordIdentities::default(),
        }
    }

    pub(crate) fn new_for_test(updates_jsonl_path: PathBuf) -> Self {
        let mut adapter = Self::new();
        adapter.updates_path = Some(updates_jsonl_path);
        adapter
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn on_session_ready(&mut self, session_id: &str, updates_jsonl_path: &Path) {
        self.session_id = session_id.to_string();
        if updates_jsonl_path.as_os_str().is_empty() {
            if let Some(resolved) = self.resolve_updates_path() {
                self.adopt_updates_path(&resolved);
            } else {
                self.baseline_ready = false;
            }
            return;
        }
        self.updates_path = Some(updates_jsonl_path.to_path_buf());
        if !updates_jsonl_path.is_file() {
            self.baseline_ready = false;
            self.committed = 0;
            return;
        }
        match complete_file_watermark(updates_jsonl_path) {
            Ok(watermark) => {
                self.committed = watermark;
                self.baseline_ready = true;
                self.file_identity = TranscriptFileIdentity::for_path(updates_jsonl_path).ok();
                self.last_visible_is_user = last_visible_committed_is_user(updates_jsonl_path);
            }
            Err(_) => {
                self.baseline_ready = false;
                self.committed = 0;
            }
        }
    }

    pub(crate) fn on_foreground_started(&mut self) {
        if self.episode.phase == EpisodePhase::Opening {
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
    ) -> GrokDispatchClaim {
        self.expire();
        if method != "session/update" && method != "_x.ai/session/update" {
            return GrokDispatchClaim::Unclaimed;
        }
        let Some(update) = params.get("update") else {
            return GrokDispatchClaim::Unclaimed;
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");

        match kind {
            "task_backgrounded" => {
                if let Some(task_id) = extract_task_id(update) {
                    self.insert_task(task_id);
                }
                GrokDispatchClaim::Accounting
            }
            "task_completed" => {
                if let Some(task_id) = extract_task_id(update) {
                    self.complete_task(task_id);
                }
                if ownership == Ownership::Idle {
                    self.last_idle_was_task_completed = true;
                }
                GrokDispatchClaim::Accounting
            }
            "user_message_chunk" => self.observe_user_chunk(update, ownership),
            "agent_message_chunk"
            | "agent_thought_chunk"
            | "tool_call"
            | "tool_call_update"
            | "auto_compact_completed" => {
                if ownership == Ownership::Idle && self.episode.is_active() {
                    self.last_idle_was_task_completed = false;
                    self.last_visible_is_user = false;
                    self.tail_once();
                    GrokDispatchClaim::AutonomousContent
                } else {
                    if ownership == Ownership::Idle {
                        self.last_idle_was_task_completed = false;
                    }
                    GrokDispatchClaim::Unclaimed
                }
            }
            "turn_completed" => {
                if ownership == Ownership::Idle && self.episode.is_active() {
                    self.last_idle_was_task_completed = false;
                    self.last_visible_is_user = false;
                    self.close_wire_episode();
                    self.tail_once();
                    GrokDispatchClaim::IdleTerminal
                } else {
                    if ownership == Ownership::Idle {
                        self.last_idle_was_task_completed = false;
                    }
                    GrokDispatchClaim::Unclaimed
                }
            }
            _ => {
                if ownership == Ownership::Idle {
                    self.last_idle_was_task_completed = false;
                }
                GrokDispatchClaim::Unclaimed
            }
        }
    }

    pub(crate) fn on_disconnect(&mut self) {
        self.tasks.clear();
        self.task_order.clear();
        self.recently_settled.clear();
        self.episode = Episode::dormant();
        self.tombstones.clear();
        self.emitted = None;
        self.last_idle_was_task_completed = false;
        self.needs_detail_refetch = false;
        self.file_identity = None;
        self.provider_record_identities.clear();
    }

    pub(crate) fn tail_once(&mut self) {
        self.expire();
        if !self.episode.is_active() {
            return;
        }
        let Some(path) = self.resolve_updates_path() else {
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
            self.reset_transcript_generation(&path, identity);
            return;
        }
        self.file_identity.get_or_insert(identity);

        if self.episode.trigger_start.is_none() {
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
            match find_hidden_trigger(&batch.bytes, self.episode.tail_from) {
                Some((offset, _, _)) if self.is_tombstoned(offset) => {
                    self.episode.phase = EpisodePhase::Closed;
                    return;
                }
                Some((offset, end, task_ids)) => {
                    if self.episode.task_ids.is_empty() {
                        self.episode.task_ids = task_ids;
                    }
                    self.episode.trigger_start = Some(offset);
                    self.episode.segment_from = end;
                    if self.episode.phase == EpisodePhase::Opening {
                        self.episode.phase = EpisodePhase::Open;
                    }
                }
                // File lags the wire: keep Opening. A from-0 tombstone close
                // would drop a new adjacent cycle that has not persisted yet.
                None => {
                    self.committed = batch.next_offset;
                    self.episode.tail_from = batch.next_offset;
                    return;
                }
            }
        }

        let Some(trigger_start) = self.episode.trigger_start else {
            return;
        };
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
            file_has_turn_completed_after(&batch.bytes, self.episode.segment_from);
        let Some(mut turn) = grok_autonomous_turn_from_segment(
            &batch.bytes,
            &self.session_id,
            self.episode.segment_from,
            trigger_start,
            &self.episode.task_ids,
        ) else {
            self.committed = batch.next_offset;
            if self.episode.phase == EpisodePhase::AwaitingPersistedTerminal && terminal_persisted {
                self.finish_episode();
            }
            return;
        };
        if turn.blocks.is_empty() {
            return;
        }
        let base_id =
            grok_autonomous_turn_id(&self.session_id, &self.episode.task_ids, trigger_start);
        let expected_id = segmented_turn_id(&base_id, self.episode.segment_part);
        turn.id = expected_id.clone();
        self.committed = batch.next_offset;
        self.episode.published_id = Some(expected_id);
        let terminal_persisted = turn.completed_at.is_some() || terminal_persisted;
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

    pub(crate) fn take_emitted(&mut self) -> GrokEmitted {
        self.take_activity().unwrap_or_else(|| GrokEmitted {
            turns: Vec::new(),
            outstanding: self.outstanding(),
            settled: Vec::new(),
            watermark: if self.baseline_ready {
                self.committed
            } else {
                0
            },
        })
    }

    pub(crate) fn take_activity(&mut self) -> Option<GrokEmitted> {
        self.emitted.take()
    }

    pub(crate) fn take_detail_refetch(&mut self) -> bool {
        let pending = self.needs_detail_refetch;
        self.needs_detail_refetch = false;
        pending
    }

    pub(crate) fn outstanding(&self) -> u32 {
        self.tasks.len() as u32
    }

    pub(crate) fn autonomous_busy(&self) -> bool {
        self.episode.is_active()
    }

    pub(crate) fn needs_periodic_tail(&self) -> bool {
        self.episode.is_active()
    }

    fn observe_user_chunk(&mut self, update: &Value, ownership: Ownership) -> GrokDispatchClaim {
        let hidden = update
            .pointer("/_meta/hideFromScrollback")
            .and_then(Value::as_bool)
            == Some(true);
        if !hidden {
            if ownership == Ownership::Idle {
                self.last_visible_is_user = true;
                self.last_idle_was_task_completed = false;
                self.log_visible_idle_user();
            }
            return GrokDispatchClaim::Unclaimed;
        }

        let text = update
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        if ownership != Ownership::Idle {
            if self.episode.phase == EpisodePhase::Opening {
                self.episode.phase = EpisodePhase::SuppressedForeground;
            }
            return GrokDispatchClaim::Unclaimed;
        }

        let adjacent = self.last_idle_was_task_completed;
        self.last_idle_was_task_completed = false;

        if self.last_visible_is_user {
            return GrokDispatchClaim::Unclaimed;
        }
        if !is_grok_background_task_reminder(text) {
            return GrokDispatchClaim::Unclaimed;
        }

        let task_ids = grok_reminder_task_ids(text);
        let matches_settled = task_ids
            .iter()
            .any(|id| self.recently_settled.iter().any(|s| s.id == *id));

        if self.episode.is_active() {
            return GrokDispatchClaim::AutonomousContent;
        }

        if self.tombstone_covers_task_ids(&task_ids) && !adjacent {
            return GrokDispatchClaim::Unclaimed;
        }

        if !matches_settled && !adjacent {
            return GrokDispatchClaim::Unclaimed;
        }

        self.consume_settled_ids(&task_ids);

        self.episode = Episode {
            phase: EpisodePhase::Opening,
            task_ids,
            trigger_start: None,
            published_id: None,
            opened_at: Instant::now(),
            tail_from: self.committed,
            segment_from: self.committed,
            segment_record_count: 0,
            segment_part: 0,
        };
        GrokDispatchClaim::AutonomousContent
    }

    fn close_wire_episode(&mut self) {
        if self.episode.is_active() {
            self.episode.phase = EpisodePhase::AwaitingPersistedTerminal;
        }
    }

    fn finish_episode(&mut self) {
        if let Some(trigger_start) = self.episode.trigger_start {
            self.tombstones.push_back(Tombstone {
                trigger_start,
                task_ids: self.episode.task_ids.clone(),
                at: Instant::now(),
            });
            while self.tombstones.len() > TOMBSTONE_CAP {
                self.tombstones.pop_front();
            }
        }
        self.episode.phase = EpisodePhase::Closed;
        self.needs_detail_refetch = true;
    }

    fn insert_task(&mut self, task_id: String) {
        if self.tasks.contains_key(&task_id) {
            return;
        }
        while self.tasks.len() >= TASK_CAP {
            if let Some(oldest) = self.task_order.pop_front() {
                self.tasks.remove(&oldest);
            } else {
                break;
            }
        }
        self.tasks.insert(
            task_id.clone(),
            TaskEntry {
                started_at: Instant::now(),
            },
        );
        self.task_order.push_back(task_id);
        self.emit_accounting();
    }

    fn complete_task(&mut self, task_id: String) {
        self.tasks.remove(&task_id);
        self.task_order.retain(|id| id != &task_id);
        self.recently_settled.retain(|s| s.id != task_id);
        self.recently_settled.push_back(SettledTask {
            id: task_id,
            at: Instant::now(),
        });
        self.emit_accounting();
    }

    fn emit_accounting(&mut self) {
        if !self.baseline_ready && !self.path_is_readable() {
            return;
        }
        self.emitted = Some(GrokEmitted {
            turns: Vec::new(),
            outstanding: self.outstanding(),
            settled: Vec::new(),
            watermark: self.committed,
        });
    }

    fn emit_turn(&mut self, turn: MessageTurn) {
        if turn.autonomous_origin != Some(AutonomousTurnOrigin::BackgroundTask) {
            return;
        }
        self.emitted = Some(GrokEmitted {
            turns: vec![turn],
            outstanding: self.outstanding(),
            settled: Vec::new(),
            watermark: self.committed,
        });
    }

    fn path_is_readable(&self) -> bool {
        self.updates_path
            .as_ref()
            .is_some_and(|path| path.is_file())
    }

    fn is_tombstoned(&self, trigger_start: u64) -> bool {
        self.tombstones
            .iter()
            .any(|t| t.trigger_start == trigger_start)
    }

    fn tombstone_covers_task_ids(&self, task_ids: &[String]) -> bool {
        !task_ids.is_empty()
            && self.tombstones.iter().any(|tombstone| {
                !tombstone.task_ids.is_empty()
                    && task_ids.iter().all(|id| tombstone.task_ids.contains(id))
            })
    }

    fn consume_settled_ids(&mut self, task_ids: &[String]) {
        self.recently_settled
            .retain(|settled| !task_ids.iter().any(|id| id == &settled.id));
    }

    fn adopt_updates_path(&mut self, path: &Path) {
        self.updates_path = Some(path.to_path_buf());
        if !path.is_file() {
            self.baseline_ready = false;
            return;
        }
        match complete_file_watermark(path) {
            Ok(watermark) => {
                self.committed = watermark;
                self.baseline_ready = true;
                self.file_identity = TranscriptFileIdentity::for_path(path).ok();
                self.last_visible_is_user = last_visible_committed_is_user(path);
            }
            Err(_) => {
                self.baseline_ready = false;
            }
        }
    }

    fn resolve_updates_path(&mut self) -> Option<PathBuf> {
        if let Some(path) = &self.updates_path {
            if path.is_file() {
                return Some(path.clone());
            }
        }
        if self.session_id.is_empty() {
            return self.updates_path.clone();
        }
        let resolved = crate::parsers::grok::grok_updates_jsonl_path(&self.session_id)?;
        self.updates_path = Some(resolved.clone());
        Some(resolved)
    }

    fn expire(&mut self) {
        let max_age = keepalive_std();
        self.tasks.retain(|id, entry| {
            let keep = entry.started_at.elapsed() < max_age;
            if !keep {
                self.task_order.retain(|existing| existing != id);
            }
            keep
        });
        self.recently_settled.retain(|s| s.at.elapsed() < max_age);
        self.tombstones.retain(|t| t.at.elapsed() < max_age);
        if self.episode.is_active() && self.episode.opened_at.elapsed() >= max_age {
            if self.episode.phase == EpisodePhase::AwaitingPersistedTerminal {
                self.needs_detail_refetch = true;
            }
            self.episode.phase = EpisodePhase::Abandoned;
        }
    }

    fn log_visible_idle_user(&mut self) {
        let now = Instant::now();
        let should_log = self
            .last_visible_user_log
            .is_none_or(|prev| now.duration_since(prev).as_secs() >= 30);
        if should_log {
            tracing::debug!(
                "[grok-autonomous] ignoring visible idle user_message_chunk (unsupported V1 shape)"
            );
            self.last_visible_user_log = Some(now);
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

    fn reset_transcript_generation(&mut self, path: &Path, identity: TranscriptFileIdentity) {
        self.episode = Episode::dormant();
        self.tombstones.clear();
        self.provider_record_identities.clear();
        self.committed = complete_file_watermark(path).unwrap_or(0);
        self.baseline_ready = true;
        self.file_identity = Some(identity);
        self.last_visible_is_user = last_visible_committed_is_user(path);
        self.needs_detail_refetch = true;
        self.emit_accounting();
    }
}

/// Prompt-receive gate used by `connection.rs`. One flag — do not invent a
/// second prompt channel.
pub(crate) fn should_hold_prompt(adapter: Option<&GrokAutonomousAdapter>) -> bool {
    adapter.is_some_and(|adapter| adapter.autonomous_busy())
}

pub(crate) fn adapter_for_connection(
    agent: AgentType,
    hidden_generation: bool,
) -> Option<GrokAutonomousAdapter> {
    if hidden_generation {
        return None;
    }
    match AutonomousActivityPolicy::for_connection(
        agent,
        &crate::acp::autonomous_activity::AutonomousCapabilities::default(),
    ) {
        AutonomousActivityPolicy::GrokIdleWire => Some(GrokAutonomousAdapter::new()),
        _ => None,
    }
}

fn extract_task_id(update: &Value) -> Option<String> {
    update
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| {
            update
                .pointer("/task_snapshot/task_id")
                .and_then(Value::as_str)
        })
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn find_hidden_trigger(bytes: &[u8], base_offset: u64) -> Option<(u64, u64, Vec<String>)> {
    let mut last_is_user = false;
    for (relative_start, record) in grok_complete_records(bytes) {
        let start = base_offset.saturating_add(relative_start);
        let Some(update) = record_update(record) else {
            continue;
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hidden = update
            .pointer("/_meta/hideFromScrollback")
            .and_then(Value::as_bool)
            == Some(true);
        if kind == "user_message_chunk" && hidden {
            if last_is_user {
                continue;
            }
            let text = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or("");
            if is_grok_background_task_reminder(text) {
                return Some((
                    start,
                    start + record.len() as u64,
                    grok_reminder_task_ids(text),
                ));
            }
            continue;
        }
        if kind == "user_message_chunk" {
            last_is_user = true;
        } else if matches!(
            kind,
            "agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "turn_completed"
        ) {
            last_is_user = false;
        }
    }
    None
}

fn file_has_turn_completed_after(bytes: &[u8], base_offset: u64) -> bool {
    grok_complete_records(bytes).any(|(start, record)| {
        let _absolute_start = base_offset.saturating_add(start);
        record_update(record)
            .and_then(|update| {
                update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .map(|kind| kind == "turn_completed")
            })
            .unwrap_or(false)
    })
}

fn segmented_turn_id(base: &str, part: u32) -> String {
    if part == 0 {
        base.to_string()
    } else {
        format!("{base}:part:{part}")
    }
}

fn record_update(record: &[u8]) -> Option<Value> {
    let payload = grok_record_payload(record);
    let value: Value = serde_json::from_slice(payload).ok()?;
    value.pointer("/params/update").cloned()
}

fn last_visible_committed_is_user(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(watermark) = complete_file_watermark(path) else {
        return false;
    };
    if watermark == 0 {
        return false;
    }
    let window = (EPISODE_PAYLOAD_MAX_BYTES as u64).saturating_mul(4);
    let from = watermark.saturating_sub(window);
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return false;
    }
    let mut bytes = vec![0u8; (watermark - from) as usize];
    if file.read_exact(&mut bytes).is_err() {
        return false;
    }
    let slice = if from == 0 {
        bytes.as_slice()
    } else {
        match bytes.iter().position(|&b| b == b'\n') {
            Some(index) => &bytes[index + 1..],
            None => return false,
        }
    };
    let records: Vec<&[u8]> = grok_complete_records(slice)
        .map(|(_, record)| record)
        .collect();
    for record in records.iter().rev().take(EPISODE_RECORD_FORCE_ROTATE) {
        let Some(update) = record_update(record) else {
            continue;
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hidden = update
            .pointer("/_meta/hideFromScrollback")
            .and_then(Value::as_bool)
            == Some(true);
        if kind == "user_message_chunk" && hidden {
            continue;
        }
        if kind == "user_message_chunk" {
            return true;
        }
        if matches!(
            kind,
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "tool_call"
                | "tool_call_update"
                | "auto_compact_completed"
                | "turn_completed"
        ) {
            return false;
        }
    }
    false
}

fn keepalive_std() -> std::time::Duration {
    background_keepalive_max_age()
        .to_std()
        .unwrap_or_else(|_| std::time::Duration::from_secs(3600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::autonomous_activity::{
        normalized_turn_payload_len, EPISODE_PAYLOAD_MAX_BYTES, EPISODE_RECORD_FORCE_ROTATE,
        PROVIDER_RECORD_IDENTITY_CAP,
    };
    use crate::models::message::{AutonomousTurnOrigin, ContentBlock};
    use crate::parsers::grok::grok_turns_from_bytes;
    use serde_json::json;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const HIDDEN_REMINDER: &str = concat!(
        r#"<system-reminder>"#,
        "\n",
        r#"Background task "term_x" completed (exit code: 0)."#,
        "\n",
        r#"</system-reminder>"#,
    );

    fn tmp_updates(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn append_line(path: &std::path::Path, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
        if !line.ends_with('\n') {
            f.write_all(b"\n").unwrap();
        }
    }

    fn jsonl_update(method: &str, update: &Value, timestamp: i64) -> String {
        json!({
            "method": method,
            "params": {"sessionId": "s", "update": update},
            "timestamp": timestamp
        })
        .to_string()
    }

    fn hidden_trigger_update() -> Value {
        json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": HIDDEN_REMINDER},
            "_meta": {"hideFromScrollback": true}
        })
    }

    fn agent_text_update(text: &str) -> Value {
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}
        })
    }

    fn turn_completed_update() -> Value {
        json!({
            "sessionUpdate": "turn_completed",
            "stop_reason": "end_turn"
        })
    }

    #[test]
    fn pathological_episode_force_rotates_and_keeps_identity_and_payload_bounds() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update": hidden_trigger_update()}),
            Ownership::Idle,
        );

        let mut transcript = String::new();
        transcript.push_str(&jsonl_update("session/update", &hidden_trigger_update(), 1));
        transcript.push('\n');
        for index in 0..(EPISODE_RECORD_FORCE_ROTATE + 1) {
            transcript.push_str(&jsonl_update(
                "session/update",
                &agent_text_update(&format!("chunk-{index};")),
                2 + index as i64,
            ));
            transcript.push('\n');
        }
        std::fs::write(&path, transcript).unwrap();

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
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update": hidden_trigger_update()}),
            Ownership::Idle,
        );
        let mut transcript = format!(
            "{}\n",
            jsonl_update("session/update", &hidden_trigger_update(), 1)
        );
        for index in 0..EPISODE_RECORD_FORCE_ROTATE {
            transcript.push_str(&jsonl_update(
                "session/update",
                &agent_text_update(&format!("chunk-{index};")),
                2 + index as i64,
            ));
            transcript.push('\n');
        }
        std::fs::write(&path, transcript).unwrap();
        adapter.tail_once();
        assert_eq!(adapter.episode.segment_record_count, 0);

        append_line(
            &path,
            &jsonl_update("session/update", &turn_completed_update(), 2000),
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update": turn_completed_update()}),
            Ownership::Idle,
        );
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_detail_refetch());
    }

    #[test]
    fn replacement_discards_episode_refetches_and_rebaselines_generation() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update": hidden_trigger_update()}),
            Ownership::Idle,
        );
        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 1),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("working"), 2),
        );
        adapter.tail_once();
        assert!(adapter.autonomous_busy());
        assert_eq!(adapter.take_emitted().turns.len(), 1);

        let replacement = path.with_extension("replacement");
        std::fs::write(
            &replacement,
            format!(
                "{}\n",
                jsonl_update("session/update", &agent_text_update("replacement"), 3)
            ),
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        adapter.tail_once();
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_detail_refetch());
        assert!(adapter.episode.trigger_start.is_none());
        assert!(adapter.episode.published_id.is_none());
        assert_eq!(adapter.provider_record_identities.len(), 0);
        assert_eq!(adapter.committed, complete_file_watermark(&path).unwrap());
    }

    #[tokio::test]
    async fn task_completed_without_trigger_creates_no_turn() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"t1"}}),
            Ownership::Idle,
        );
        assert!(adapter.take_emitted().turns.is_empty());
        assert_eq!(adapter.outstanding(), 0);
    }

    #[tokio::test]
    async fn hidden_trigger_then_persisted_assistant_upserts_one_stable_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);

        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        assert!(
            adapter.take_emitted().turns.is_empty(),
            "no emit before persist"
        );

        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 9),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("hello"), 10),
        );
        adapter.tail_once();
        let first = adapter.take_emitted();
        assert_eq!(first.turns.len(), 1);
        assert_eq!(
            first.turns[0].autonomous_origin,
            Some(AutonomousTurnOrigin::BackgroundTask)
        );
        assert!(first.settled.is_empty(), "Grok V1 settled stays empty");
        assert!(first.watermark > 0, "do not emit an unwatermarked turn");
        let id = first.turns[0].id.clone();
        assert_eq!(
            id,
            crate::parsers::grok::grok_autonomous_turn_id("sess", &["term_x".into()], 0)
        );

        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update(" world"), 11),
        );
        adapter.tail_once();
        let second = adapter.take_emitted();
        assert_eq!(second.turns[0].id, id);
        assert!(
            matches!(&second.turns[0].blocks[0], ContentBlock::Text { text } if text.contains("hello"))
        );
    }

    #[test]
    fn visible_user_chunk_does_not_open_episode() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":"hello from another client"}
            }}),
            Ownership::Idle,
        );
        assert!(!adapter.autonomous_busy());
        assert!(adapter.take_emitted().turns.is_empty());
    }

    #[test]
    fn foreground_ownership_suppresses_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Foreground,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Foreground,
        );
        assert!(!adapter.autonomous_busy());
        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 1),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("no overlay"), 2),
        );
        adapter.tail_once();
        assert!(adapter.take_emitted().turns.is_empty());
    }

    #[test]
    fn duplicate_task_launch_is_idempotent() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path);
        let params = json!({"update":{
            "sessionUpdate":"task_backgrounded",
            "task_id":"term_x",
            "tool_call_id":"call-1"
        }});
        adapter.on_raw_dispatch("_x.ai/session/update", &params, Ownership::Foreground);
        adapter.on_raw_dispatch("_x.ai/session/update", &params, Ownership::Foreground);
        assert_eq!(adapter.outstanding(), 1);
    }

    #[test]
    fn unknown_completion_does_not_underflow() {
        let (_dir, path) = tmp_updates("");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"ghost"}}),
            Ownership::Idle,
        );
        assert_eq!(adapter.outstanding(), 0);
    }

    #[test]
    fn prompt_gate_holds_until_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        assert!(adapter.autonomous_busy());
        assert!(should_hold_prompt(Some(&adapter)));
        assert!(!should_hold_prompt(None));

        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 9),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("hello"), 10),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &turn_completed_update(), 11),
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}),
            Ownership::Idle,
        );
        adapter.tail_once();
        assert!(!adapter.autonomous_busy());
        assert!(!should_hold_prompt(Some(&adapter)));
    }

    #[test]
    fn hidden_trigger_after_committed_user_is_not_autonomous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":"please continue"}
            }}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        assert!(!adapter.autonomous_busy());

        append_line(
            &path,
            &jsonl_update(
                "session/update",
                &json!({
                    "sessionUpdate":"user_message_chunk",
                    "content":{"type":"text","text":"please continue"},
                    "_meta":{"promptIndex":2}
                }),
                1,
            ),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 2),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("user reply"), 3),
        );
        adapter.tail_once();
        assert!(adapter.take_emitted().turns.is_empty());
    }

    #[test]
    fn attach_with_trailing_visible_user_does_not_open_on_reminder() {
        let user_line = jsonl_update(
            "session/update",
            &json!({
                "sessionUpdate": "user_message_chunk",
                "content": {"type": "text", "text": "please continue"},
                "_meta": {"promptIndex": 2}
            }),
            1,
        );
        let hidden_line = jsonl_update("session/update", &hidden_trigger_update(), 2);
        let (_dir, path) = tmp_updates(&format!("{user_line}\n{hidden_line}\n"));
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        assert!(
            !adapter.autonomous_busy(),
            "reattach must recover a trailing visible User and reject the reminder"
        );
        assert!(adapter.take_emitted().turns.is_empty());
    }

    #[test]
    fn idle_turn_completed_is_claimed_as_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        let claim = adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}),
            Ownership::Idle,
        );
        assert!(claim.is_idle_terminal());
        assert!(claim.skip_streaming_reducer());
    }

    #[test]
    fn missing_transcript_does_not_fabricate_content_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        let emitted = adapter.take_emitted();
        assert!(emitted.turns.is_empty());
        assert_eq!(emitted.watermark, 0);
    }

    #[test]
    fn replayed_hidden_reminder_after_close_does_not_hold_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 9),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("hello"), 10),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &turn_completed_update(), 11),
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}),
            Ownership::Idle,
        );
        adapter.tail_once();
        assert!(
            !adapter.autonomous_busy(),
            "closed after persisted terminal"
        );
        let _ = adapter.take_emitted();

        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        adapter.tail_once();
        assert!(
            !adapter.autonomous_busy(),
            "replayed hidden reminder must not pin the prompt gate"
        );
        assert!(!should_hold_prompt(Some(&adapter)));
    }

    #[test]
    fn second_cycle_while_file_lags_stays_opening_and_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("sess", &path);
        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        append_line(
            &path,
            &jsonl_update("session/update", &hidden_trigger_update(), 9),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &agent_text_update("hello"), 10),
        );
        append_line(
            &path,
            &jsonl_update("session/update", &turn_completed_update(), 11),
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}}),
            Ownership::Idle,
        );
        adapter.tail_once();
        assert!(
            !adapter.autonomous_busy(),
            "first cycle closed after persisted terminal"
        );

        adapter.on_raw_dispatch(
            "_x.ai/session/update",
            &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
            Ownership::Idle,
        );
        adapter.on_raw_dispatch(
            "session/update",
            &json!({"update":{
                "sessionUpdate":"user_message_chunk",
                "content":{"type":"text","text":HIDDEN_REMINDER},
                "_meta":{"hideFromScrollback":true}
            }}),
            Ownership::Idle,
        );
        assert!(
            adapter.autonomous_busy(),
            "adjacent second cycle opens before the file appends"
        );
        adapter.tail_once();
        assert!(
            adapter.autonomous_busy(),
            "second cycle while file lags must stay Opening / busy, not Closed"
        );
        assert!(should_hold_prompt(Some(&adapter)));
    }

    #[test]
    fn missing_then_created_updates_jsonl_recovers() {
        let home = tempfile::tempdir().unwrap();
        let session_id = "019f45e3-e1ef-7690-a29f-fe2554382b49";
        let session_dir = home.path().join("sessions").join("%2Ftmp").join(session_id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("updates.jsonl");

        with_temp_grok_home(home.path(), || {
            let mut adapter = GrokAutonomousAdapter::new();
            adapter.on_session_ready(session_id, Path::new(""));
            adapter.on_raw_dispatch(
                "_x.ai/session/update",
                &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
                Ownership::Idle,
            );
            adapter.on_raw_dispatch(
                "session/update",
                &json!({"update":{
                    "sessionUpdate":"user_message_chunk",
                    "content":{"type":"text","text":HIDDEN_REMINDER},
                    "_meta":{"hideFromScrollback":true}
                }}),
                Ownership::Idle,
            );
            assert!(adapter.autonomous_busy());
            adapter.tail_once();
            assert!(
                adapter.take_emitted().turns.is_empty(),
                "no emit while transcript is missing"
            );

            append_line(
                &path,
                &jsonl_update("session/update", &hidden_trigger_update(), 9),
            );
            append_line(
                &path,
                &jsonl_update("session/update", &agent_text_update("hello"), 10),
            );
            adapter.tail_once();
            let emitted = adapter.take_emitted();
            assert_eq!(emitted.turns.len(), 1, "later append must recover");
            assert_eq!(
                emitted.turns[0].autonomous_origin,
                Some(AutonomousTurnOrigin::BackgroundTask)
            );
            assert!(emitted.watermark > 0);
        });
    }

    const GROK_SESSION_3806: &str = include_str!("fixtures/grok_autonomous_session_3806.jsonl");

    fn grok_fixture_lines() -> Vec<&'static str> {
        GROK_SESSION_3806
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    fn is_grok_task_completed_line(line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        value
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("task_completed")
    }

    fn blocks_contain_system_reminder(blocks: &[ContentBlock]) -> bool {
        blocks.iter().any(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
                text.contains("system-reminder")
            }
            ContentBlock::ToolUse { input_preview, .. } => input_preview
                .as_deref()
                .is_some_and(|text| text.contains("system-reminder")),
            ContentBlock::ToolResult { output_preview, .. } => output_preview
                .as_deref()
                .is_some_and(|text| text.contains("system-reminder")),
            _ => false,
        })
    }

    #[test]
    fn grok_session_3806_fixture_emits_one_marked_turn_and_covering_watermark() {
        let lines = grok_fixture_lines();
        assert!(
            !lines.is_empty(),
            "grok_autonomous_session_3806.jsonl must contain the redacted session-3806 sequence"
        );

        let split = lines
            .iter()
            .position(|line| is_grok_task_completed_line(line))
            .expect("fixture must start the idle sequence at task_completed");
        let preamble = &lines[..split];
        let idle = &lines[split..];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        for line in preamble {
            append_line(&path, line);
        }

        let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
        adapter.on_session_ready("session-3806", &path);

        let mut stable_id: Option<String> = None;
        let mut last_overlay_watermark = 0u64;
        let mut upserts = 0usize;
        let mut terminal_claim: Option<GrokDispatchClaim> = None;

        for line in idle {
            append_line(&path, line);
            let value: Value = serde_json::from_str(line).expect("fixture line is JSON");
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .expect("fixture line has method");
            let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
            let kind = params
                .pointer("/update/sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("");
            let claim = adapter.on_raw_dispatch(method, &params, Ownership::Idle);
            if kind == "turn_completed" {
                assert!(
                    claim.is_idle_terminal(),
                    "idle turn_completed must not emit a foreground TurnComplete"
                );
                assert!(
                    claim.skip_streaming_reducer(),
                    "idle terminal must skip the foreground finalize path"
                );
                terminal_claim = Some(claim);
            }

            let emitted = adapter.take_emitted();
            assert!(emitted.settled.is_empty(), "Grok settled stays empty");
            for turn in emitted.turns {
                assert_eq!(
                    turn.autonomous_origin,
                    Some(AutonomousTurnOrigin::BackgroundTask)
                );
                assert!(
                    !blocks_contain_system_reminder(&turn.blocks),
                    "hidden system-reminder must not appear in emitted blocks"
                );
                match &stable_id {
                    None => stable_id = Some(turn.id.clone()),
                    Some(id) => assert_eq!(
                        turn.id, *id,
                        "incremental upserts must keep one stable turn id"
                    ),
                }
                upserts += 1;
                last_overlay_watermark = emitted.watermark;
            }
        }

        let id = stable_id.expect("one marked assistant incrementally upserted");
        assert!(id.starts_with("grok-autonomous:"));
        assert!(
            upserts >= 2,
            "thought/message/tool updates must upsert the same turn more than once"
        );
        assert!(
            terminal_claim.is_some_and(GrokDispatchClaim::is_idle_terminal),
            "no foreground TurnComplete signal from the adapter"
        );
        assert!(
            adapter.take_detail_refetch(),
            "final refetch requested after idle terminal"
        );
        assert!(!adapter.autonomous_busy());

        let bytes = std::fs::read(&path).unwrap();
        let (turns, parser_watermark) = grok_turns_from_bytes(&bytes, "session-3806");
        assert!(
            parser_watermark >= last_overlay_watermark,
            "parser watermark {parser_watermark} must cover last overlay watermark {last_overlay_watermark}"
        );
        let autos: Vec<&MessageTurn> = turns
            .iter()
            .filter(|turn| turn.autonomous_origin == Some(AutonomousTurnOrigin::BackgroundTask))
            .collect();
        assert_eq!(autos.len(), 1);
        assert_eq!(autos[0].id, id);
        assert!(
            turns
                .iter()
                .all(|turn| !blocks_contain_system_reminder(&turn.blocks)),
            "cold parse must also omit system-reminder"
        );
    }

    fn with_temp_grok_home<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
        use std::ffi::OsString;
        use std::sync::{Mutex, OnceLock};
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
}
