use std::collections::VecDeque;

use sacp::UntypedMessage;

const MAX_FAILED_WINDOWS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokRetryAction {
    Pass,
    Consume,
    Rollback { attempt: u32 },
    DropStale { update_kind: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamIdentity {
    prompt_id: String,
    stream_start_ms: u64,
}

#[derive(Debug, Clone)]
struct FailedWindow {
    prompt_id: Option<String>,
    stream: Option<StreamIdentity>,
    retry_event_seq: u64,
    retry_agent_timestamp_ms: u64,
}

#[derive(Debug)]
struct RetryMarker {
    event_seq: u64,
    agent_timestamp_ms: u64,
    attempt: u32,
}

#[derive(Debug)]
struct StandardUpdate {
    kind: &'static str,
    event_seq: Option<u64>,
    agent_timestamp_ms: Option<u64>,
    prompt_id: Option<String>,
    stream_start_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct GrokRetryReconciler {
    active_prompt_id: Option<String>,
    active_stream: Option<StreamIdentity>,
    failed: VecDeque<FailedWindow>,
    speculative_output: bool,
}

impl GrokRetryReconciler {
    pub fn observe(&mut self, notification: &UntypedMessage) -> GrokRetryAction {
        if let Some(marker) = parse_retry_marker(notification) {
            return self.observe_retry(marker);
        }
        let Some(update) = parse_standard_update(notification) else {
            return GrokRetryAction::Pass;
        };
        self.observe_standard(update)
    }

    fn observe_retry(&mut self, marker: RetryMarker) -> GrokRetryAction {
        self.failed.push_back(FailedWindow {
            prompt_id: self.active_prompt_id.clone(),
            stream: self.active_stream.clone(),
            retry_event_seq: marker.event_seq,
            retry_agent_timestamp_ms: marker.agent_timestamp_ms,
        });
        while self.failed.len() > MAX_FAILED_WINDOWS {
            self.failed.pop_front();
        }

        if std::mem::take(&mut self.speculative_output) {
            GrokRetryAction::Rollback {
                attempt: marker.attempt,
            }
        } else {
            GrokRetryAction::Consume
        }
    }

    fn observe_standard(&mut self, update: StandardUpdate) -> GrokRetryAction {
        if self.is_stale(&update) {
            return GrokRetryAction::DropStale {
                update_kind: update.kind,
            };
        }

        if let Some(prompt_id) = update.prompt_id.clone() {
            self.active_prompt_id = Some(prompt_id.clone());
            if let Some(stream_start_ms) = update.stream_start_ms {
                self.active_stream = Some(StreamIdentity {
                    prompt_id,
                    stream_start_ms,
                });
            }
        }
        self.speculative_output = update.kind != "tool_call";
        GrokRetryAction::Pass
    }

    fn is_stale(&self, update: &StandardUpdate) -> bool {
        let Some(event_seq) = update.event_seq else {
            return false;
        };

        self.failed.iter().any(|failed| {
            if event_seq >= failed.retry_event_seq {
                return false;
            }

            match &failed.stream {
                Some(stream) => {
                    update.prompt_id.as_deref() == Some(stream.prompt_id.as_str())
                        && update.stream_start_ms == Some(stream.stream_start_ms)
                }
                None => {
                    failed.prompt_id.is_some()
                        && update.prompt_id == failed.prompt_id
                        && update
                            .agent_timestamp_ms
                            .is_some_and(|timestamp| timestamp <= failed.retry_agent_timestamp_ms)
                }
            }
        })
    }
}

fn parse_retry_marker(notification: &UntypedMessage) -> Option<RetryMarker> {
    if notification.method() != "_x.ai/session/update" {
        return None;
    }
    let params = notification.params();
    let update = params.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "retry_state"
        || update.get("type")?.as_str()? != "retrying"
    {
        return None;
    }
    let meta = params.get("_meta")?;
    Some(RetryMarker {
        event_seq: parse_event_sequence(meta.get("eventId")?.as_str()?)?,
        agent_timestamp_ms: meta.get("agentTimestampMs")?.as_u64()?,
        attempt: u32::try_from(update.get("attempt")?.as_u64()?).ok()?,
    })
}

fn parse_standard_update(notification: &UntypedMessage) -> Option<StandardUpdate> {
    if notification.method() != "session/update" {
        return None;
    }
    let params = notification.params();
    let kind = rollbackable_kind(params.get("update")?.get("sessionUpdate")?.as_str()?)?;
    let meta = params.get("_meta");
    Some(StandardUpdate {
        kind,
        event_seq: meta
            .and_then(|value| value.get("eventId"))
            .and_then(|value| value.as_str())
            .and_then(parse_event_sequence),
        agent_timestamp_ms: meta
            .and_then(|value| value.get("agentTimestampMs"))
            .and_then(|value| value.as_u64()),
        prompt_id: meta
            .and_then(|value| value.get("promptId"))
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        stream_start_ms: meta
            .and_then(|value| value.get("streamStartMs"))
            .and_then(|value| value.as_u64()),
    })
}

fn rollbackable_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "agent_message_chunk" => Some("agent_message_chunk"),
        "agent_thought_chunk" => Some("agent_thought_chunk"),
        "plan" => Some("plan"),
        "tool_call" => Some("tool_call"),
        _ => None,
    }
}

fn parse_event_sequence(event_id: &str) -> Option<u64> {
    event_id.rsplit_once('-')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sacp::UntypedMessage;

    fn standard(
        kind: &str,
        event_seq: u64,
        agent_timestamp_ms: u64,
        prompt_id: &str,
        stream_start_ms: u64,
    ) -> UntypedMessage {
        UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": kind,
                    "content": { "type": "text", "text": "candidate" }
                },
                "_meta": {
                    "eventId": format!("session-1-{event_seq}"),
                    "agentTimestampMs": agent_timestamp_ms,
                    "promptId": prompt_id,
                    "streamStartMs": stream_start_ms
                }
            }),
        )
        .expect("standard notification")
    }

    fn retry(event_seq: u64, agent_timestamp_ms: u64, attempt: u32) -> UntypedMessage {
        retry_state("retrying", event_seq, agent_timestamp_ms, Some(attempt))
    }

    fn retry_state(
        retry_type: &str,
        event_seq: u64,
        agent_timestamp_ms: u64,
        attempt: Option<u32>,
    ) -> UntypedMessage {
        UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "retry_state",
                    "type": retry_type,
                    "attempt": attempt,
                    "max_retries": 15,
                    "reason": "provider unavailable"
                },
                "_meta": {
                    "eventId": format!("session-1-{event_seq}"),
                    "agentTimestampMs": agent_timestamp_ms
                }
            }),
        )
        .expect("retry notification")
    }

    fn tool_update(event_seq: u64) -> UntypedMessage {
        UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tool-1",
                    "status": "completed"
                },
                "_meta": {
                    "eventId": format!("session-1-{event_seq}"),
                    "agentTimestampMs": 1_100,
                    "promptId": "prompt-1",
                    "streamStartMs": 100
                }
            }),
        )
        .expect("tool update")
    }

    #[test]
    fn retry_rolls_back_failed_stream_and_drops_its_late_message() {
        let mut reconciler = GrokRetryReconciler::default();

        assert_eq!(
            reconciler.observe(&standard("agent_thought_chunk", 21, 1_000, "prompt-1", 100)),
            GrokRetryAction::Pass
        );
        assert_eq!(
            reconciler.observe(&retry(32, 1_100, 1)),
            GrokRetryAction::Rollback { attempt: 1 }
        );
        assert_eq!(
            reconciler.observe(&standard("agent_message_chunk", 31, 1_100, "prompt-1", 100)),
            GrokRetryAction::DropStale {
                update_kind: "agent_message_chunk"
            }
        );
        assert_eq!(
            reconciler.observe(&standard("agent_thought_chunk", 51, 2_000, "prompt-1", 200)),
            GrokRetryAction::Pass
        );
        assert_eq!(
            reconciler.observe(&standard("agent_message_chunk", 61, 2_100, "prompt-1", 200)),
            GrokRetryAction::Pass
        );
    }

    #[test]
    fn stale_window_does_not_drop_another_prompt_or_newer_event() {
        let mut reconciler = GrokRetryReconciler::default();
        reconciler.observe(&standard("agent_thought_chunk", 21, 1_000, "prompt-1", 100));
        reconciler.observe(&retry(32, 1_100, 1));

        assert_eq!(
            reconciler.observe(&standard("agent_message_chunk", 31, 1_100, "prompt-2", 100)),
            GrokRetryAction::Pass
        );
        assert_eq!(
            reconciler.observe(&standard("agent_message_chunk", 61, 2_100, "prompt-1", 100)),
            GrokRetryAction::Pass
        );
    }

    #[test]
    fn missing_standard_metadata_fails_open() {
        let mut reconciler = GrokRetryReconciler::default();
        reconciler.observe(&standard("agent_thought_chunk", 21, 1_000, "prompt-1", 100));
        reconciler.observe(&retry(32, 1_100, 1));
        let missing_meta = UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "ambiguous" }
                }
            }),
        )
        .expect("missing-meta notification");

        assert_eq!(reconciler.observe(&missing_meta), GrokRetryAction::Pass);
    }

    #[test]
    fn terminal_retry_states_and_tool_updates_are_ignored() {
        let mut reconciler = GrokRetryReconciler::default();

        assert_eq!(
            reconciler.observe(&retry_state("failed", 32, 1_100, None)),
            GrokRetryAction::Pass
        );
        assert_eq!(
            reconciler.observe(&retry_state("exhausted", 33, 1_200, None)),
            GrokRetryAction::Pass
        );
        assert_eq!(reconciler.observe(&tool_update(31)), GrokRetryAction::Pass);
    }

    #[test]
    fn repeated_retry_without_new_output_is_consumed_without_second_rollback() {
        let mut reconciler = GrokRetryReconciler::default();
        reconciler.observe(&standard("agent_thought_chunk", 21, 1_000, "prompt-1", 100));

        assert_eq!(
            reconciler.observe(&retry(32, 1_100, 1)),
            GrokRetryAction::Rollback { attempt: 1 }
        );
        assert_eq!(
            reconciler.observe(&retry(33, 1_200, 2)),
            GrokRetryAction::Consume
        );
    }

    #[test]
    fn failed_window_history_is_bounded() {
        let mut reconciler = GrokRetryReconciler::default();
        for attempt in 1..=20 {
            reconciler.observe(&standard(
                "agent_thought_chunk",
                u64::from(attempt) * 10,
                u64::from(attempt) * 100,
                "prompt-1",
                u64::from(attempt),
            ));
            reconciler.observe(&retry(
                u64::from(attempt) * 10 + 1,
                u64::from(attempt) * 100 + 1,
                attempt,
            ));
        }

        assert_eq!(reconciler.failed.len(), MAX_FAILED_WINDOWS);
    }
}
