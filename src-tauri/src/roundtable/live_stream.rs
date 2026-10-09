//! Display-only live output of in-flight attempts.
//!
//! Message and thinking chunks an agent streams over ACP are mirrored here so
//! the room page can show them while the attempt runs. This is process-local
//! memory only: it is never persisted, never hashed into a projection, never
//! read by acceptance, validation or publication, and never becomes a
//! published message. The verified message replaces it once the attempt is
//! accepted and its phase published. Buffers are bounded per attempt and per
//! room, redacted against the attempt's registered secrets before anyone can
//! read them, and dropped when the phase publishes or the run ends.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

/// Message text is kept from the start (a published result is far smaller).
pub(crate) const MESSAGE_CAP_BYTES: usize = 32 * 1024;
/// Thinking keeps the most recent bytes; earlier thinking scrolls out.
pub(crate) const THOUGHT_CAP_BYTES: usize = 16 * 1024;
/// At most this many attempts are kept per room (oldest ended dropped first).
const ATTEMPTS_PER_ROOM: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveKind {
    Message,
    Thought,
}

struct Redactor {
    secrets: Vec<String>,
    held: String,
}

impl Redactor {
    fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|secret| secret.len() >= 6);
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        Self {
            secrets,
            held: String::new(),
        }
    }

    /// Returns text that can no longer change under redaction. A suffix that
    /// could still be the start of a secret split across chunks is held back.
    fn push(&mut self, text: &str) -> String {
        let mut combined = std::mem::take(&mut self.held);
        combined.push_str(text);
        for secret in &self.secrets {
            if combined.contains(secret.as_str()) {
                combined = combined.replace(secret.as_str(), "[redacted]");
            }
        }
        let mut keep_from = combined.len();
        for secret in &self.secrets {
            let longest = secret.len().saturating_sub(1).min(combined.len());
            for len in (1..=longest).rev() {
                let start = combined.len() - len;
                if combined.is_char_boundary(start) && secret.starts_with(&combined[start..]) {
                    keep_from = keep_from.min(start);
                    break;
                }
            }
        }
        self.held = combined.split_off(keep_from);
        combined
    }

    fn finish(&mut self) -> String {
        std::mem::take(&mut self.held)
    }
}

struct LiveAttempt {
    order: u64,
    speaker_id: String,
    phase_id: String,
    phase_kind: String,
    incarnation: String,
    message: String,
    message_truncated: bool,
    message_redactor: Redactor,
    /// Retained thinking tail and the absolute offset of its first byte.
    thought: String,
    thought_start: u64,
    thought_redactor: Redactor,
    activity: Option<String>,
    ended: bool,
    version: u64,
}

impl LiveAttempt {
    fn thought_end(&self) -> u64 {
        self.thought_start + self.thought.len() as u64
    }

    fn append(&mut self, kind: LiveKind, text: &str) {
        match kind {
            LiveKind::Message => {
                let clean = self.message_redactor.push(text);
                self.append_message(&clean);
            }
            LiveKind::Thought => {
                let clean = self.thought_redactor.push(text);
                self.append_thought(&clean);
            }
        }
    }

    fn append_message(&mut self, clean: &str) {
        if clean.is_empty() || self.message_truncated {
            return;
        }
        let room = MESSAGE_CAP_BYTES.saturating_sub(self.message.len());
        if clean.len() <= room {
            self.message.push_str(clean);
        } else {
            let mut cut = room;
            while !clean.is_char_boundary(cut) {
                cut -= 1;
            }
            self.message.push_str(&clean[..cut]);
            self.message_truncated = true;
        }
        self.version += 1;
    }

    fn append_thought(&mut self, clean: &str) {
        if clean.is_empty() {
            return;
        }
        self.thought.push_str(clean);
        if self.thought.len() > THOUGHT_CAP_BYTES {
            let mut cut = self.thought.len() - THOUGHT_CAP_BYTES;
            while !self.thought.is_char_boundary(cut) {
                cut += 1;
            }
            self.thought.drain(..cut);
            self.thought_start += cut as u64;
        }
        self.version += 1;
    }
}

#[derive(Default)]
struct RoomLive {
    next_order: u64,
    attempts: BTreeMap<String, LiveAttempt>,
}

fn registry() -> &'static Mutex<HashMap<String, RoomLive>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, RoomLive>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_registry<T>(f: impl FnOnce(&mut HashMap<String, RoomLive>) -> T) -> T {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    f(&mut guard)
}

/// Write handle for one attempt. Dropping it marks the attempt ended; the
/// buffer stays readable until its phase publishes or the run ends.
pub(crate) struct LiveSink {
    room: String,
    attempt: String,
}

impl LiveSink {
    pub(crate) fn begin(
        room: &str,
        attempt: &str,
        speaker: &str,
        phase: &str,
        phase_kind: &str,
        incarnation: &str,
        secrets: Vec<String>,
    ) -> Arc<Self> {
        with_registry(|rooms| {
            let live = rooms.entry(room.to_owned()).or_default();
            live.next_order += 1;
            let order = live.next_order;
            live.attempts.insert(
                attempt.to_owned(),
                LiveAttempt {
                    order,
                    speaker_id: speaker.to_owned(),
                    phase_id: phase.to_owned(),
                    phase_kind: phase_kind.to_owned(),
                    incarnation: incarnation.to_owned(),
                    message: String::new(),
                    message_truncated: false,
                    message_redactor: Redactor::new(secrets.clone()),
                    thought: String::new(),
                    thought_start: 0,
                    thought_redactor: Redactor::new(secrets),
                    activity: None,
                    ended: false,
                    version: 1,
                },
            );
            while live.attempts.len() > ATTEMPTS_PER_ROOM {
                let oldest = live
                    .attempts
                    .iter()
                    .filter(|(_, item)| item.ended)
                    .min_by_key(|(_, item)| item.order)
                    .or_else(|| live.attempts.iter().min_by_key(|(_, item)| item.order))
                    .map(|(id, _)| id.clone());
                match oldest {
                    Some(id) => {
                        live.attempts.remove(&id);
                    }
                    None => break,
                }
            }
        });
        Arc::new(Self {
            room: room.to_owned(),
            attempt: attempt.to_owned(),
        })
    }

    fn update(&self, f: impl FnOnce(&mut LiveAttempt)) {
        with_registry(|rooms| {
            if let Some(attempt) = rooms
                .get_mut(&self.room)
                .and_then(|live| live.attempts.get_mut(&self.attempt))
            {
                if !attempt.ended {
                    f(attempt);
                }
            }
        });
    }

    pub(crate) fn push(&self, kind: LiveKind, text: &str) {
        if text.is_empty() {
            return;
        }
        self.update(|attempt| attempt.append(kind, text));
    }

    /// Short status such as the tool being called. Display only.
    pub(crate) fn activity(&self, label: &str) {
        let label: String = label.chars().take(120).collect();
        self.update(|attempt| {
            if attempt.activity.as_deref() != Some(label.as_str()) {
                attempt.activity = Some(label);
                attempt.version += 1;
            }
        });
    }

    pub(crate) fn end(&self) {
        self.update(|attempt| {
            let rest = attempt.message_redactor.finish();
            attempt.append_message(&rest);
            let rest = attempt.thought_redactor.finish();
            attempt.append_thought(&rest);
            attempt.ended = true;
            attempt.version += 1;
        });
    }
}

impl Drop for LiveSink {
    fn drop(&mut self) {
        self.end();
    }
}

/// The phase is published: its verified messages replace the live buffers.
pub(crate) fn clear_phase(room: &str, phase: &str) {
    with_registry(|rooms| {
        if let Some(live) = rooms.get_mut(room) {
            live.attempts.retain(|_, attempt| attempt.phase_id != phase);
            if live.attempts.is_empty() {
                rooms.remove(room);
            }
        }
    });
}

/// The run ended (completed, paused, stopped or failed).
pub(crate) fn clear_room(room: &str) {
    with_registry(|rooms| {
        rooms.remove(room);
    });
}

/// What one subscriber has already been sent, per attempt.
#[derive(Default)]
pub struct LiveCursor {
    sent: HashMap<String, (String, u64, usize, u64)>,
}

/// Frames for one room since `cursor`. The first call for a subscriber (a new
/// attach, a reconnect or a page refresh) sends each attempt's whole current
/// buffer; later calls send only appended text. Offsets are byte offsets into
/// the attempt's message (from 0) and thinking (absolute, the head may have
/// scrolled out). Returns `None` when nothing changed.
pub fn live_frame(room: &str, cursor: &mut LiveCursor) -> Option<Value> {
    with_registry(|rooms| {
        let empty = RoomLive::default();
        let live = rooms.get(room).unwrap_or(&empty);
        let mut attempts = Vec::new();
        let mut ordered: Vec<_> = live.attempts.iter().collect();
        ordered.sort_by_key(|(_, attempt)| attempt.order);
        for (id, attempt) in ordered {
            let previous = cursor.sent.get(id);
            if previous.is_some_and(|(incarnation, version, _, _)| {
                *version == attempt.version && incarnation == &attempt.incarnation
            }) {
                continue;
            }
            let (message_from, thought_from) = match previous {
                Some((incarnation, _, message_sent, thought_sent))
                    if incarnation == &attempt.incarnation
                        && *message_sent <= attempt.message.len()
                        && *thought_sent >= attempt.thought_start
                        && *thought_sent <= attempt.thought_end() =>
                {
                    (*message_sent, *thought_sent)
                }
                _ => (0, attempt.thought_start),
            };
            let thought_local = (thought_from - attempt.thought_start) as usize;
            attempts.push(json!({
                "attempt_id": id,
                "speaker_id": attempt.speaker_id,
                "phase_id": attempt.phase_id,
                "phase_kind": attempt.phase_kind,
                "incarnation": attempt.incarnation,
                "ended": attempt.ended,
                "activity": attempt.activity,
                "message": {
                    "offset": message_from,
                    "text": &attempt.message[message_from..],
                    "truncated": attempt.message_truncated,
                },
                "thought": {
                    "offset": thought_from,
                    "text": &attempt.thought[thought_local..],
                    "start": attempt.thought_start,
                },
            }));
            cursor.sent.insert(
                id.clone(),
                (
                    attempt.incarnation.clone(),
                    attempt.version,
                    attempt.message.len(),
                    attempt.thought_end(),
                ),
            );
        }
        let removed: Vec<String> = cursor
            .sent
            .keys()
            .filter(|id| !live.attempts.contains_key(*id))
            .cloned()
            .collect();
        for id in &removed {
            cursor.sent.remove(id);
        }
        if attempts.is_empty() && removed.is_empty() {
            return None;
        }
        Some(json!({
            "type": "roundtable_live",
            "room_id": room,
            "verified": false,
            "attempts": attempts,
            "removed": removed,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(tag: &str) -> String {
        format!("room-{tag}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn subscribers_get_the_whole_buffer_first_then_only_deltas() {
        let room = room("delta");
        let sink = LiveSink::begin(&room, "a1", "s1", "p1", "proposal", "i1", Vec::new());
        sink.push(LiveKind::Thought, "thinking ");
        sink.push(LiveKind::Message, "Hello ");
        let mut first = LiveCursor::default();
        let frame = live_frame(&room, &mut first).unwrap();
        assert_eq!(frame["verified"], false);
        assert_eq!(frame["attempts"][0]["message"]["text"], "Hello ");
        assert_eq!(frame["attempts"][0]["thought"]["text"], "thinking ");
        assert!(live_frame(&room, &mut first).is_none());
        sink.push(LiveKind::Message, "world");
        let delta = live_frame(&room, &mut first).unwrap();
        assert_eq!(delta["attempts"][0]["message"]["offset"], 6);
        assert_eq!(delta["attempts"][0]["message"]["text"], "world");
        assert_eq!(delta["attempts"][0]["thought"]["text"], "");
        // A refresh/reconnect is a new subscriber: whole current buffer.
        let mut refreshed = LiveCursor::default();
        let full = live_frame(&room, &mut refreshed).unwrap();
        assert_eq!(full["attempts"][0]["message"]["offset"], 0);
        assert_eq!(full["attempts"][0]["message"]["text"], "Hello world");
        clear_room(&room);
        let gone = live_frame(&room, &mut first).unwrap();
        assert_eq!(gone["removed"][0], "a1");
    }

    #[test]
    fn concurrent_members_stream_independently_and_phase_publish_clears_them() {
        let room = room("multi");
        let grok = LiveSink::begin(&room, "a1", "s1", "p1", "proposal", "i1", Vec::new());
        let anti = LiveSink::begin(&room, "a2", "s2", "p1", "proposal", "i2", Vec::new());
        grok.push(LiveKind::Message, "G");
        anti.push(LiveKind::Message, "A");
        let mut cursor = LiveCursor::default();
        let frame = live_frame(&room, &mut cursor).unwrap();
        assert_eq!(frame["attempts"].as_array().unwrap().len(), 2);
        drop(grok);
        let ended = live_frame(&room, &mut cursor).unwrap();
        assert_eq!(ended["attempts"][0]["ended"], true);
        let synth = LiveSink::begin(&room, "a3", "s3", "p2", "synthesis", "i3", Vec::new());
        synth.push(LiveKind::Message, "S");
        clear_phase(&room, "p1");
        let after = live_frame(&room, &mut cursor).unwrap();
        let removed = after["removed"].as_array().unwrap().to_vec();
        assert!(removed.contains(&json!("a1")) && removed.contains(&json!("a2")));
        assert_eq!(after["attempts"][0]["attempt_id"], "a3");
        clear_room(&room);
    }

    #[test]
    fn buffers_are_capped_and_secrets_split_across_chunks_are_masked() {
        let room = room("cap");
        let secret = "attempt-secret-0123456789";
        let sink = LiveSink::begin(
            &room,
            "a1",
            "s1",
            "p1",
            "synthesis",
            "i1",
            vec![secret.into()],
        );
        sink.push(LiveKind::Message, "key attempt-secret-01");
        sink.push(LiveKind::Message, "23456789 end");
        // 3000 x 12 bytes passes the 32 KiB message cap; 30000 bytes of
        // thinking passes its 16 KiB tail cap.
        for _ in 0..3_000 {
            sink.push(LiveKind::Thought, "0123456789");
            sink.push(LiveKind::Message, "é0123456789");
        }
        let mut cursor = LiveCursor::default();
        let frame = live_frame(&room, &mut cursor).unwrap();
        let message = frame["attempts"][0]["message"]["text"].as_str().unwrap();
        assert!(
            message.starts_with("key [redacted] end"),
            "{}",
            &message[..40]
        );
        assert!(!message.contains("attempt-secret"));
        assert!(message.len() <= MESSAGE_CAP_BYTES);
        assert_eq!(frame["attempts"][0]["message"]["truncated"], true);
        let thought = frame["attempts"][0]["thought"]["text"].as_str().unwrap();
        assert!(thought.len() <= THOUGHT_CAP_BYTES);
        assert_eq!(
            frame["attempts"][0]["thought"]["start"].as_u64().unwrap() + thought.len() as u64,
            30_000
        );
        clear_room(&room);
    }

    #[test]
    fn a_held_secret_prefix_is_released_when_it_turns_out_harmless() {
        let room = room("held");
        let sink = LiveSink::begin(
            &room,
            "a1",
            "s1",
            "p1",
            "critique",
            "i1",
            vec!["zebra-secret-1".into()],
        );
        sink.push(LiveKind::Message, "a zebra-");
        let mut cursor = LiveCursor::default();
        let frame = live_frame(&room, &mut cursor).unwrap();
        assert_eq!(frame["attempts"][0]["message"]["text"], "a ");
        sink.push(LiveKind::Message, "crossing");
        let frame = live_frame(&room, &mut cursor).unwrap();
        assert_eq!(frame["attempts"][0]["message"]["text"], "zebra-crossing");
        sink.end();
        clear_room(&room);
    }
}
