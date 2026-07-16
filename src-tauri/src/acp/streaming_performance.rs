//! Desktop ACP streaming performance flags and capability types.
//!
//! Pure normalization and environment parsing for the three internal
//! controls. Live mode ownership and batch delivery land in later tasks;
//! P0 keeps defaults on the legacy single-event path.

use serde::{Deserialize, Serialize};

/// Which desktop delivery path is active for ACP events to the webview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDeliveryMode {
    Legacy,
    Batched,
}

/// Three internal streaming-performance controls.
///
/// Invalid combinations normalize downward:
/// - incremental transcript requires batching
/// - deferred rich content requires incremental transcript
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamingPerformanceFlags {
    pub desktop_acp_event_batching: bool,
    pub incremental_live_transcript: bool,
    pub deferred_streaming_rich_content: bool,
}

impl StreamingPerformanceFlags {
    /// All controls off — single-event legacy emit path.
    pub fn legacy() -> Self {
        Self {
            desktop_acp_event_batching: false,
            incremental_live_transcript: false,
            deferred_streaming_rich_content: false,
        }
    }

    /// Collapse invalid combinations so dependents never outrank their
    /// prerequisites.
    pub fn normalized(mut self) -> Self {
        if !self.desktop_acp_event_batching {
            self.incremental_live_transcript = false;
        }
        if !self.incremental_live_transcript {
            self.deferred_streaming_rich_content = false;
        }
        self
    }

    /// Phase default when env vars are absent or invalid.
    ///
    /// P4 (Task 15): all three flags default on after P3 gate; explicit env
    /// false values and downward normalization remain available for opt-out.
    pub fn phase_default() -> Self {
        Self {
            desktop_acp_event_batching: true,
            incremental_live_transcript: true,
            deferred_streaming_rich_content: true,
        }
    }

    /// Parse flags from process environment, then normalize.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Pure lookup-based parse for tests and env injection.
    ///
    /// Tri-state: true is `1|true|yes|on`, false is `0|false|no|off`
    /// (ASCII case-insensitive after trim). Absent/invalid keeps the phase
    /// default for that flag and logs only the variable name.
    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut flags = Self::phase_default();
        apply_env_flag(
            &mut flags.desktop_acp_event_batching,
            "CODEG_DESKTOP_ACP_EVENT_BATCHING",
            &mut lookup,
        );
        apply_env_flag(
            &mut flags.incremental_live_transcript,
            "CODEG_INCREMENTAL_LIVE_TRANSCRIPT",
            &mut lookup,
        );
        apply_env_flag(
            &mut flags.deferred_streaming_rich_content,
            "CODEG_DEFERRED_STREAMING_RICH_CONTENT",
            &mut lookup,
        );
        flags.normalized()
    }
}

fn apply_env_flag<F>(slot: &mut bool, name: &str, lookup: &mut F)
where
    F: FnMut(&str) -> Option<String>,
{
    let Some(raw) = lookup(name) else {
        return;
    };
    match parse_bool_env(&raw) {
        Some(value) => *slot = value,
        None => {
            tracing::warn!("[ACP] invalid value for {name}");
        }
    }
}

fn parse_bool_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Capability snapshot for desktop ACP delivery diagnostics.
///
/// Task 5 supplies the live mode owner; this type is the stable wire shape.
#[derive(Debug, Clone, Serialize)]
pub struct DesktopDeliveryCapabilities {
    pub mode: DesktopDeliveryMode,
    pub flags: StreamingPerformanceFlags,
    pub perf_replay_available: bool,
    pub failure_event: &'static str,
}

impl DesktopDeliveryCapabilities {
    pub const FAILURE_EVENT: &'static str = "acp://delivery-failed";

    /// Legacy single-event delivery with all performance flags off.
    pub fn legacy() -> Self {
        Self {
            mode: DesktopDeliveryMode::Legacy,
            flags: StreamingPerformanceFlags::legacy(),
            perf_replay_available: false,
            failure_event: Self::FAILURE_EVENT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_flag_combinations_normalize_downward() {
        let flags = StreamingPerformanceFlags {
            desktop_acp_event_batching: false,
            incremental_live_transcript: true,
            deferred_streaming_rich_content: true,
        }
        .normalized();

        assert_eq!(
            flags,
            StreamingPerformanceFlags {
                desktop_acp_event_batching: false,
                incremental_live_transcript: false,
                deferred_streaming_rich_content: false,
            }
        );
    }

    #[test]
    fn release_defaults_enable_the_complete_path() {
        let flags = StreamingPerformanceFlags::from_lookup(|_| None);
        assert!(flags.desktop_acp_event_batching);
        assert!(flags.incremental_live_transcript);
        assert!(flags.deferred_streaming_rich_content);
    }

    #[test]
    fn from_lookup_uses_phase_default_when_absent() {
        let flags = StreamingPerformanceFlags::from_lookup(|_| None);
        assert_eq!(flags, StreamingPerformanceFlags::phase_default());
    }

    #[test]
    fn disabling_batching_disables_dependent_paths() {
        let flags = StreamingPerformanceFlags::from_lookup(|name| {
            (name == "CODEG_DESKTOP_ACP_EVENT_BATCHING").then_some("0".into())
        });
        assert_eq!(flags, StreamingPerformanceFlags::legacy());
    }

    #[test]
    fn from_lookup_parses_tri_state_and_normalizes() {
        let flags = StreamingPerformanceFlags::from_lookup(|name| match name {
            "CODEG_DESKTOP_ACP_EVENT_BATCHING" => Some("0".into()),
            "CODEG_INCREMENTAL_LIVE_TRANSCRIPT" => Some("true".into()),
            "CODEG_DEFERRED_STREAMING_RICH_CONTENT" => Some("YES".into()),
            _ => None,
        });
        // batching false forces dependents off despite env true values
        assert_eq!(flags, StreamingPerformanceFlags::legacy());
    }

    #[test]
    fn explicit_false_keeps_opt_out_of_single_flag() {
        let flags = StreamingPerformanceFlags::from_lookup(|name| {
            (name == "CODEG_DEFERRED_STREAMING_RICH_CONTENT").then_some("off".into())
        });
        assert!(flags.desktop_acp_event_batching);
        assert!(flags.incremental_live_transcript);
        assert!(!flags.deferred_streaming_rich_content);
    }
}
