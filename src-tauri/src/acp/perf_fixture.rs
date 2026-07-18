//! Deterministic ACP streaming performance fixture and timed replay driver.
//!
//! Available under unit tests and the `test-utils` feature only. Production
//! desktop/server binaries leave this module uncompiled so the synthetic
//! workload and replay surface cannot ship by accident.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::acp::error::AcpError;
use crate::acp::session_state::SessionState;
use crate::acp::types::{AcpEvent, ConnectionStatus, PlanEntryInfo};
use crate::web::event_bridge::{emit_with_state, EventEmitter};

const TARGET_TEXT_CHARS: usize = 30_000;
const CONTENT_CHUNKS: usize = 1_000;
const TOOL_CALLS: usize = 51;
const FIXTURE_VERSION: &str = "grok-rich-v1";

const PREFIX: &str = concat!(
    "# Streaming fixture\n\n",
    "English prose before CJK fast.\n",
    "中文流式输出用于验证 WebView2 在快速回复时不会停顿后跳跃。\n\n",
    "```rust\n",
    "fn main() {\n    for i in 0..2048 {\n",
    "        println!(\"frame {i}\");\n    }\n}\n",
    "```\n\n",
    "| index | value | 状态 |\n",
    "| ---: | :--- | :--- |\n",
    "| 1 | alpha | 运行中 |\n",
    "| 2 | beta | 完成 |\n\n",
    "Inline math $a^2+b^2=c^2$ and display math:\n",
    "$$\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n$$\n\n",
    "```mermaid\nsequenceDiagram\n",
    "    participant A as Agent\n",
    "    participant W as WebView\n",
    "    A->>W: event batch\n",
    "    W-->>A: paint\n```\n\n",
);

const FILLER: &str =
    "Fast Grok output keeps prose, 中文字符, `inline code`, and table-like | cells | moving.\n";

/// Predefined fixture identifiers accepted by the replay command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerfFixtureId {
    GrokRichV1,
}

/// Fixed envelope-rate profiles. Offsets are absolute from t=0 so late ticks
/// never reorder events; tests assert the schedule without sleeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerfRateProfile {
    #[serde(rename = "eps_100")]
    Eps100,
    #[serde(rename = "eps_500")]
    Eps500,
    #[serde(rename = "eps_1000")]
    Eps1000,
}

impl PerfRateProfile {
    /// Inter-event period for this profile.
    fn interval(self) -> Duration {
        match self {
            Self::Eps100 => Duration::from_millis(10),
            Self::Eps500 => Duration::from_millis(2),
            Self::Eps1000 => Duration::from_millis(1),
        }
    }

    /// Absolute schedule offsets for `count` events (index 0 at t=0).
    pub fn offsets(self, count: usize) -> Vec<Duration> {
        let step = self.interval();
        (0..count).map(|i| step * (i as u32)).collect()
    }
}

/// Built fixture payload used by unit tests and the timed replay driver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PerfFixture {
    pub version: String,
    pub final_text: String,
    pub final_text_sha256: String,
    pub events: Vec<AcpEvent>,
    pub tool_call_count: usize,
    pub code_fence_is_split_across_chunks: bool,
}

/// Request body for `acp_replay_streaming_perf_fixture`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PerfReplayRequest {
    pub fixture_id: PerfFixtureId,
    pub seed: u64,
    pub rate_profile: PerfRateProfile,
}

/// Result returned after a timed fixture replay completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PerfReplayResult {
    pub version: String,
    pub event_count: u64,
    pub tool_call_count: u64,
    pub final_text_sha256: String,
    pub final_event_seq: u64,
    pub elapsed_ms: u64,
}

fn fixture_text() -> String {
    let mut text = PREFIX.to_owned();
    while text.chars().count() < TARGET_TEXT_CHARS {
        text.push_str(FILLER);
    }
    text.chars().take(TARGET_TEXT_CHARS).collect()
}

fn content_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(TARGET_TEXT_CHARS / CONTENT_CHUNKS)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// True when the opening ``` of a fence straddles a 30-char chunk boundary
/// (first Rust fence starts at character 88 → ends of chunk 2 has two
/// backticks, chunk 3 continues with the third).
fn code_fence_is_split(chunks: &[String]) -> bool {
    chunks
        .windows(2)
        .any(|pair| pair[0].ends_with("``") && pair[1].starts_with('`'))
}

fn tool_call(id: &str, index: usize, seed: u64) -> AcpEvent {
    AcpEvent::ToolCall {
        tool_call_id: id.to_owned(),
        title: format!("perf command {index}"),
        kind: "execute".into(),
        status: "in_progress".into(),
        content: None,
        raw_input: Some(
            serde_json::json!({
                "command": format!("fixture-step-{index}"),
                "nonce": seed ^ index as u64,
            })
            .to_string(),
        ),
        raw_output: None,
        locations: None,
        meta: None,
        images: None,
    }
}

fn tool_append(id: &str, output: String) -> AcpEvent {
    AcpEvent::ToolCallUpdate {
        tool_call_id: id.to_owned(),
        title: None,
        status: None,
        content: None,
        raw_input: None,
        raw_output: Some(output),
        raw_output_append: Some(true),
        locations: None,
        meta: None,
        images: None,
    }
}

fn tool_complete(id: &str, output: String) -> AcpEvent {
    AcpEvent::ToolCallUpdate {
        tool_call_id: id.to_owned(),
        title: None,
        status: Some("completed".into()),
        content: None,
        raw_input: None,
        raw_output: Some(output),
        raw_output_append: Some(true),
        locations: None,
        meta: None,
        images: None,
    }
}

fn permission_request() -> AcpEvent {
    AcpEvent::PermissionRequest {
        request_id: "perf-permission".into(),
        tool_call: serde_json::json!({
            "toolCallId": "perf-tool-12",
            "title": "Synthetic permission",
        }),
        options: vec![],
    }
}

fn build_events(final_text: &str, seed: u64) -> (Vec<AcpEvent>, usize, bool) {
    let chunks = content_chunks(final_text);
    let fence_split = code_fence_is_split(&chunks);

    let mut events = vec![AcpEvent::StatusChanged {
        status: ConnectionStatus::Prompting,
    }];
    let mut tool_index = 0usize;

    for (index, text) in chunks.into_iter().enumerate() {
        events.push(AcpEvent::ContentDelta { text });

        if index % 20 == 19 {
            events.push(AcpEvent::Thinking {
                text: format!("thinking-{index}\n"),
            });
        }
        if index % 19 == 18 && tool_index < TOOL_CALLS {
            let id = format!("perf-tool-{tool_index:02}");
            events.push(tool_call(&id, tool_index, seed));
            events.push(tool_append(&id, format!("chunk-{tool_index}\n")));
            events.push(tool_complete(&id, format!("done-{tool_index}\n")));
            tool_index += 1;
        }
        if index % 100 == 99 {
            events.push(AcpEvent::PlanUpdate {
                entries: vec![PlanEntryInfo {
                    content: format!("phase-{}", index / 100),
                    priority: "medium".into(),
                    status: "in_progress".into(),
                }],
            });
        }
        if index == 249 {
            events.push(permission_request());
            events.push(AcpEvent::PermissionResolved {
                request_id: "perf-permission".into(),
            });
        }
        if index == 499 {
            events.push(AcpEvent::QuestionRequest {
                question_id: "perf-question".into(),
                questions: vec![],
            });
            events.push(AcpEvent::QuestionResolved {
                question_id: "perf-question".into(),
            });
        }
        if index % 250 == 249 {
            events.push(AcpEvent::UsageUpdate {
                used: (index + 1) as u64,
                size: 200_000,
            });
        }
    }

    events.push(AcpEvent::TurnComplete {
        session_id: "perf-grok-rich-v1".into(),
        stop_reason: "end_turn".into(),
        agent_type: "grok".into(),
        mark_awaiting_reply: false,
    });

    debug_assert_eq!(events.len(), 1_223);
    debug_assert_eq!(tool_index, TOOL_CALLS);

    (events, tool_index, fence_split)
}

/// Build the fixed-contract fixture for `id` with seed-varying tool payloads.
pub fn build_perf_fixture(id: PerfFixtureId, seed: u64) -> PerfFixture {
    match id {
        PerfFixtureId::GrokRichV1 => {
            let final_text = fixture_text();
            let final_text_sha256 = sha256_hex(&final_text);
            let (events, tool_call_count, code_fence_is_split_across_chunks) =
                build_events(&final_text, seed);
            PerfFixture {
                version: FIXTURE_VERSION.to_owned(),
                final_text,
                final_text_sha256,
                events,
                tool_call_count,
                code_fence_is_split_across_chunks,
            }
        }
    }
}

/// Drive fixture events through the normal `emit_with_state` path on absolute
/// schedule offsets so late ticks never reorder delivery.
pub async fn replay_perf_fixture(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request: PerfReplayRequest,
) -> Result<PerfReplayResult, AcpError> {
    let fixture = build_perf_fixture(request.fixture_id, request.seed);
    let event_count = fixture.events.len();
    let offsets = request.rate_profile.offsets(event_count);
    let version = fixture.version.clone();
    let final_text_sha256 = fixture.final_text_sha256.clone();
    let tool_call_count = fixture.tool_call_count as u64;

    let start = Instant::now();
    let tokio_start = tokio::time::Instant::now();
    for (i, event) in fixture.events.into_iter().enumerate() {
        let deadline = tokio_start + offsets[i];
        tokio::time::sleep_until(deadline).await;
        emit_with_state(state, emitter, event).await;
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let final_event_seq = state.read().await.event_seq;

    Ok(PerfReplayResult {
        version,
        event_count: event_count as u64,
        tool_call_count,
        final_text_sha256,
        final_event_seq,
        elapsed_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::acp::session_state::{LiveContentBlock, LiveSessionSnapshot, ToolCallOutput};
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

    #[test]
    fn grok_rich_v1_has_fixed_contract_and_checksum() {
        let fixture = build_perf_fixture(PerfFixtureId::GrokRichV1, 0xC0DE);
        assert_eq!(fixture.version, "grok-rich-v1");
        assert_eq!(fixture.final_text.chars().count(), 30_000);
        assert_eq!(fixture.events.len(), 1_223);
        assert_eq!(fixture.tool_call_count, 51);
        assert_eq!(
            fixture.final_text_sha256,
            "65380735c9a752758c7bace17cc722d86400480a0ae1dff62759f37eafa4b039"
        );
        assert!(fixture.final_text.contains("中文流式输出"));
        assert!(fixture.final_text.contains("```mermaid"));
        assert!(fixture.code_fence_is_split_across_chunks);
    }

    #[test]
    fn schedules_are_exact_and_do_not_sleep() {
        let count = 1_223;
        assert_eq!(
            PerfRateProfile::Eps100.offsets(count)[1],
            Duration::from_millis(10)
        );
        assert_eq!(
            PerfRateProfile::Eps500.offsets(count)[1],
            Duration::from_millis(2)
        );
        assert_eq!(
            PerfRateProfile::Eps1000.offsets(count)[1],
            Duration::from_millis(1)
        );
        assert_eq!(PerfRateProfile::Eps1000.offsets(count).len(), count);
    }

    #[test]
    fn seed_changes_synthetic_tool_payloads_but_not_the_text_contract() {
        let first = build_perf_fixture(PerfFixtureId::GrokRichV1, 1);
        let second = build_perf_fixture(PerfFixtureId::GrokRichV1, 2);
        assert_ne!(
            serde_json::to_value(&first.events).unwrap(),
            serde_json::to_value(&second.events).unwrap()
        );
        assert_eq!(first.final_text_sha256, second.final_text_sha256);
        assert_eq!(first.events.len(), second.events.len());
    }

    fn joined_snapshot_text(snapshot: &LiveSessionSnapshot) -> String {
        snapshot
            .live_message
            .as_ref()
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        LiveContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn replay_uses_normal_state_sequence_and_ring_path() {
        let manager = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster);
        manager
            .insert_test_connection("perf-c1", AgentType::Grok, None, emitter.clone())
            .await;
        let state = manager.get_state("perf-c1").await.expect("state");
        let fixture = build_perf_fixture(PerfFixtureId::GrokRichV1, 0xC0DE);

        for event in fixture.events[..fixture.events.len() - 1].iter().cloned() {
            emit_with_state(&state, &emitter, event).await;
        }
        let snapshot = state.read().await.to_snapshot();
        assert_eq!(snapshot.event_seq, 1_222);
        assert_eq!(snapshot.active_tool_calls.len(), 51);
        assert_eq!(joined_snapshot_text(&snapshot).chars().count(), 30_000);
        let last_tool = snapshot
            .active_tool_calls
            .iter()
            .find(|tool| tool.id == "perf-tool-50")
            .expect("final tool");
        assert!(matches!(
            &last_tool.output,
            Some(ToolCallOutput::Text { content })
                if content == "chunk-50\ndone-50\n"
        ));

        emit_with_state(&state, &emitter, fixture.events.last().unwrap().clone()).await;
        let completed = state.read().await;
        assert_eq!(completed.event_seq, 1_223);
        assert!(completed.live_message.is_none());
        assert!(completed.active_tool_calls.is_empty());
    }
}
