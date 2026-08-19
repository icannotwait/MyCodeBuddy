use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::models::agent::AgentType;
use crate::models::message::{ContentBlock, MessageTurn};

pub(crate) const PROVIDER_RECORD_IDENTITY_CAP: usize = 1_024;
pub(crate) const EPISODE_RECORD_ROTATE: usize = 512;
pub(crate) const EPISODE_RECORD_FORCE_ROTATE: usize = 1_024;
pub(crate) const EPISODE_PAYLOAD_MAX_BYTES: usize = 2 * 1024 * 1024;
const EPISODE_RAW_BATCH_MAX_BYTES: usize = 4 * EPISODE_PAYLOAD_MAX_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptFileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    created: Option<std::time::SystemTime>,
}

impl TranscriptFileIdentity {
    fn from_file(file: &File) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            use std::mem::MaybeUninit;
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };

            let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
            // SAFETY: `file` owns a valid handle for the duration of the call,
            // and `info` points to writable storage of the required type.
            let result = unsafe {
                GetFileInformationByHandle(file.as_raw_handle() as HANDLE, info.as_mut_ptr())
            };
            if result == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: a successful call initialized the complete structure.
            let info = unsafe { info.assume_init() };
            Ok(Self {
                volume_serial: info.dwVolumeSerialNumber,
                file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                created: file.metadata()?.created().ok(),
            })
        }
    }

    pub(crate) fn for_path(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        Self::from_file(&file)
    }
}

pub(crate) struct CompleteRecordBatch {
    pub(crate) bytes: Vec<u8>,
    pub(crate) record_starts: Vec<u64>,
    pub(crate) next_offset: u64,
    pub(crate) skipped_oversized_record: bool,
}

pub(crate) fn read_complete_record_batch(
    path: &Path,
    from: u64,
) -> std::io::Result<CompleteRecordBatch> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();
    if from > file_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "transcript cursor is beyond file length",
        ));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut bytes = Vec::new();
    let mut record_starts = Vec::new();
    let mut record_ends = Vec::new();
    let mut cursor = from;
    let mut record_start = from;
    let mut record_buffer_start = 0usize;
    let mut skipping_oversized = false;

    while record_starts.len() < EPISODE_RECORD_FORCE_ROTATE {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(buffer.len(), |index| index + 1);

        if !skipping_oversized && bytes.len().saturating_add(take) > EPISODE_RAW_BATCH_MAX_BYTES {
            if !record_starts.is_empty() {
                bytes.truncate(record_buffer_start);
                return Ok(CompleteRecordBatch {
                    bytes,
                    record_starts,
                    next_offset: record_start,
                    skipped_oversized_record: false,
                });
            }
            bytes.clear();
            skipping_oversized = true;
        }
        if !skipping_oversized {
            bytes.extend_from_slice(&buffer[..take]);
        }
        reader.consume(take);
        cursor = cursor.saturating_add(take as u64);

        if newline.is_some() {
            if skipping_oversized {
                return Ok(CompleteRecordBatch {
                    bytes: Vec::new(),
                    record_starts: Vec::new(),
                    next_offset: cursor,
                    skipped_oversized_record: true,
                });
            }
            record_starts.push(record_start);
            record_ends.push(cursor);
            record_start = cursor;
            record_buffer_start = bytes.len();
        }
    }

    // A trailing fragment is not committed and will be read again after its newline arrives.
    bytes.truncate(record_buffer_start);
    let next_offset = record_ends.last().copied().unwrap_or(from);
    Ok(CompleteRecordBatch {
        bytes,
        record_starts,
        next_offset,
        skipped_oversized_record: false,
    })
}

pub(crate) fn complete_file_watermark(path: &Path) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(0);
    }
    let mut end = len;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let start = end.saturating_sub(buffer.len() as u64);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        std::io::Read::read_exact(&mut file, &mut buffer[..count])?;
        if let Some(index) = buffer[..count].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + index as u64 + 1);
        }
        if start == 0 {
            return Ok(0);
        }
        end = start;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeRotation {
    Boundary,
    Forced,
}

pub(crate) fn rotation_decision(
    record_count: usize,
    at_episode_boundary: bool,
) -> Option<EpisodeRotation> {
    if record_count >= EPISODE_RECORD_FORCE_ROTATE {
        Some(EpisodeRotation::Forced)
    } else if record_count >= EPISODE_RECORD_ROTATE && at_episode_boundary {
        Some(EpisodeRotation::Boundary)
    } else {
        None
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProviderRecordIdentities {
    order: VecDeque<String>,
    entries: HashSet<String>,
}

impl ProviderRecordIdentities {
    pub(crate) fn remember(&mut self, identity: String) {
        if !self.entries.insert(identity.clone()) {
            self.order.retain(|existing| existing != &identity);
        }
        self.order.push_back(identity);
        while self.order.len() > PROVIDER_RECORD_IDENTITY_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, identity: &str) -> bool {
        self.entries.contains(identity)
    }

    pub(crate) fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
    }
}

pub(crate) fn normalized_turn_payload_len(turn: &MessageTurn) -> usize {
    serde_json::to_vec(turn).map_or(usize::MAX, |payload| payload.len())
}

pub(crate) fn cap_normalized_turn_payload(mut turn: MessageTurn) -> Option<MessageTurn> {
    while normalized_turn_payload_len(&turn) > EPISODE_PAYLOAD_MAX_BYTES {
        let overflow = normalized_turn_payload_len(&turn) - EPISODE_PAYLOAD_MAX_BYTES;
        let block = turn.blocks.last_mut()?;
        let reduced = match block {
            ContentBlock::Text { text } | ContentBlock::Thinking { text } => {
                truncate_payload_string(text, overflow.saturating_add(256))
            }
            ContentBlock::Image { .. } => false,
            ContentBlock::ImageGeneration {
                revised_prompt,
                image,
            } => {
                if image.take().is_some() {
                    true
                } else {
                    revised_prompt.take().is_some()
                }
            }
            ContentBlock::ToolUse {
                input_preview,
                meta,
                ..
            } => {
                if input_preview.take().is_some() {
                    true
                } else {
                    meta.take().is_some()
                }
            }
            ContentBlock::ToolResult {
                output_preview,
                agent_stats,
                images,
                ..
            } => {
                if output_preview.take().is_some() {
                    true
                } else if !images.is_empty() {
                    images.clear();
                    true
                } else {
                    agent_stats.take().is_some()
                }
            }
        };
        if !reduced {
            turn.blocks.pop();
        }
    }
    (!turn.blocks.is_empty()).then_some(turn)
}

fn truncate_payload_string(value: &mut String, remove_bytes: usize) -> bool {
    if value.is_empty() {
        return false;
    }
    let mut target = value.len().saturating_sub(remove_bytes.max(1));
    while target > 0 && !value.is_char_boundary(target) {
        target -= 1;
    }
    value.truncate(target);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomousActivityPolicy {
    ClaudeTranscript,
    GrokIdleWire,
    CodexGoalTranscript,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutonomousCapabilities {
    pub goal_version: Option<u32>,
    pub load_session: bool,
}

impl AutonomousActivityPolicy {
    pub fn for_connection(agent: AgentType, caps: &AutonomousCapabilities) -> Self {
        match agent {
            AgentType::ClaudeCode => Self::ClaudeTranscript,
            AgentType::Grok => Self::GrokIdleWire,
            AgentType::Codex if caps.goal_version == Some(1) && caps.load_session => {
                Self::CodexGoalTranscript
            }
            _ => Self::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cap_normalized_turn_payload, normalized_turn_payload_len, rotation_decision,
        AutonomousActivityPolicy, AutonomousCapabilities, EpisodeRotation,
        ProviderRecordIdentities, EPISODE_PAYLOAD_MAX_BYTES, EPISODE_RECORD_FORCE_ROTATE,
        EPISODE_RECORD_ROTATE, PROVIDER_RECORD_IDENTITY_CAP,
    };
    use crate::models::agent::AgentType;
    use crate::models::message::{ContentBlock, MessageTurn, TurnRole};
    use chrono::Utc;

    #[test]
    fn claude_maps_to_transcript() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::ClaudeCode,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::ClaudeTranscript
        );
    }

    #[test]
    fn grok_maps_to_idle_wire() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Grok,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::GrokIdleWire
        );
    }

    #[test]
    fn codex_requires_goal_v1_and_load_session() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        assert_eq!(
            AutonomousActivityPolicy::for_connection(AgentType::Codex, &qualified),
            AutonomousActivityPolicy::CodexGoalTranscript
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(1),
                    load_session: false,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(2),
                    load_session: true,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::Unsupported
        );
    }

    #[test]
    fn custom_codex_and_other_builtins_are_unsupported() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        for agent in [
            AgentType::Cursor,
            AgentType::OpenCode,
            AgentType::Gemini,
            AgentType::Cline,
            AgentType::Hermes,
            AgentType::CodeBuddy,
            AgentType::KimiCode,
            AgentType::Pi,
            AgentType::DeepSeek,
            AgentType::Custom("codex"),
        ] {
            assert_eq!(
                AutonomousActivityPolicy::for_connection(agent, &qualified),
                AutonomousActivityPolicy::Unsupported,
                "{agent:?}"
            );
        }
    }

    #[test]
    fn provider_record_identity_lru_never_exceeds_1024() {
        let mut identities = ProviderRecordIdentities::default();
        for index in 0..(PROVIDER_RECORD_IDENTITY_CAP + 17) {
            identities.remember(format!("provider-record-{index}"));
        }
        assert_eq!(identities.len(), PROVIDER_RECORD_IDENTITY_CAP);
        assert!(!identities.contains("provider-record-0"));
        assert!(identities.contains("provider-record-1024"));
    }

    #[test]
    fn episode_rotation_waits_for_a_boundary_after_512_and_forces_at_1024() {
        assert_eq!(rotation_decision(EPISODE_RECORD_ROTATE - 1, true), None);
        assert_eq!(rotation_decision(EPISODE_RECORD_ROTATE, false), None);
        assert_eq!(
            rotation_decision(EPISODE_RECORD_ROTATE, true),
            Some(EpisodeRotation::Boundary)
        );
        assert_eq!(
            rotation_decision(EPISODE_RECORD_FORCE_ROTATE, false),
            Some(EpisodeRotation::Forced)
        );
    }

    #[test]
    fn normalized_episode_turn_payload_is_hard_capped_at_two_mib() {
        let turn = MessageTurn {
            id: "oversized-autonomous".into(),
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::Text {
                text: "x".repeat(EPISODE_PAYLOAD_MAX_BYTES + 4096),
            }],
            timestamp: Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            reasoning_effort: None,
            completed_at: None,
            outcome: None,
            autonomous_origin: None,
        };

        let capped = cap_normalized_turn_payload(turn).expect("retain a bounded prefix");
        assert!(normalized_turn_payload_len(&capped) <= EPISODE_PAYLOAD_MAX_BYTES);
        assert!(!capped.blocks.is_empty());
    }
}
