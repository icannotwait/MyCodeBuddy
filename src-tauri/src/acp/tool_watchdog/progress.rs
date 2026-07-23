//! Bounded semantic-progress fingerprints for tool-execution leases.
//!
//! Only fixed-size facts are retained (offsets, enum/hash fingerprints,
//! monotonic activity timestamps). No tool output text is kept.

use std::collections::BTreeMap;

use super::registry::SemanticProgress;

/// Per-lease progress baseline used to detect *new* semantic facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressFingerprint {
    /// Unkeyed / single-channel terminal offset (no terminal identity).
    pub terminal_offset: Option<u64>,
    /// Per-terminal offsets keyed by stable hash of terminal id. Multi-terminal
    /// tools renew when *any* associated terminal advances its own offset.
    pub per_terminal_offsets: BTreeMap<u64, u64>,
    pub terminal_exited: bool,
    pub tool_status_fingerprint: Option<u64>,
    pub mcp_token_or_hash: Option<u64>,
    pub delegation_at_mono_ms: Option<u64>,
    pub agent_content_hash: Option<u64>,
}

/// Apply a semantic progress fact. Returns `true` only when the fact is new
/// relative to the retained fingerprint (renews the lease progress window).
pub fn apply_semantic_progress(
    fingerprint: &mut ProgressFingerprint,
    fact: &SemanticProgress,
) -> bool {
    match fact {
        SemanticProgress::TerminalOffset {
            terminal_id_hash: Some(hash),
            next_offset,
        } => match fingerprint.per_terminal_offsets.get(hash) {
            Some(prev) if *next_offset <= *prev => false,
            _ => {
                fingerprint.per_terminal_offsets.insert(*hash, *next_offset);
                true
            }
        },
        SemanticProgress::TerminalOffset {
            terminal_id_hash: None,
            next_offset,
        } => match fingerprint.terminal_offset {
            Some(prev) if *next_offset <= prev => false,
            _ => {
                fingerprint.terminal_offset = Some(*next_offset);
                true
            }
        },
        SemanticProgress::TerminalExit => {
            if fingerprint.terminal_exited {
                false
            } else {
                fingerprint.terminal_exited = true;
                true
            }
        }
        SemanticProgress::ToolStatusChanged {
            status_fingerprint,
        } => {
            if fingerprint.tool_status_fingerprint == Some(*status_fingerprint) {
                false
            } else {
                fingerprint.tool_status_fingerprint = Some(*status_fingerprint);
                true
            }
        }
        SemanticProgress::McpProgress { token_or_hash } => {
            if fingerprint.mcp_token_or_hash == Some(*token_or_hash) {
                false
            } else {
                fingerprint.mcp_token_or_hash = Some(*token_or_hash);
                true
            }
        }
        SemanticProgress::DelegationActivity { at_mono_ms } => {
            match fingerprint.delegation_at_mono_ms {
                Some(prev) if *at_mono_ms <= prev => false,
                _ => {
                    fingerprint.delegation_at_mono_ms = Some(*at_mono_ms);
                    true
                }
            }
        }
        SemanticProgress::AgentActivity { content_hash } => {
            if fingerprint.agent_content_hash == Some(*content_hash) {
                false
            } else {
                fingerprint.agent_content_hash = Some(*content_hash);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_terminal_offset_does_not_renew() {
        let mut fp = ProgressFingerprint::default();
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: None,
                next_offset: 10
            }
        ));
        assert!(!apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: None,
                next_offset: 10
            }
        ));
        assert!(!apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: None,
                next_offset: 5
            }
        ));
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: None,
                next_offset: 11
            }
        ));
    }

    #[test]
    fn multi_terminal_lower_offset_peer_still_renews() {
        let mut fp = ProgressFingerprint::default();
        // Terminal A reaches a high offset.
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: Some(0xA),
                next_offset: 1000
            }
        ));
        // Terminal B advances from 10 → 20. Max-offset comparison would miss
        // this; per-terminal tracking must renew.
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: Some(0xB),
                next_offset: 10
            }
        ));
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: Some(0xB),
                next_offset: 20
            }
        ));
        // Unchanged B offset does not renew.
        assert!(!apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: Some(0xB),
                next_offset: 20
            }
        ));
        // A's own further advance still renews independently.
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalOffset {
                terminal_id_hash: Some(0xA),
                next_offset: 1001
            }
        ));
    }

    #[test]
    fn terminal_exit_only_once() {
        let mut fp = ProgressFingerprint::default();
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalExit
        ));
        assert!(!apply_semantic_progress(
            &mut fp,
            &SemanticProgress::TerminalExit
        ));
    }

    #[test]
    fn duplicate_status_fingerprint_does_not_renew() {
        let mut fp = ProgressFingerprint::default();
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::ToolStatusChanged {
                status_fingerprint: 7
            }
        ));
        assert!(!apply_semantic_progress(
            &mut fp,
            &SemanticProgress::ToolStatusChanged {
                status_fingerprint: 7
            }
        ));
        assert!(apply_semantic_progress(
            &mut fp,
            &SemanticProgress::ToolStatusChanged {
                status_fingerprint: 8
            }
        ));
    }

    #[test]
    fn no_output_text_in_fingerprint_debug() {
        let fp = ProgressFingerprint {
            terminal_offset: Some(42),
            per_terminal_offsets: BTreeMap::from([(1, 99)]),
            terminal_exited: false,
            tool_status_fingerprint: Some(0xabc),
            mcp_token_or_hash: Some(9),
            delegation_at_mono_ms: Some(1000),
            agent_content_hash: Some(0xdead),
        };
        let debug = format!("{fp:?}");
        assert!(!debug.contains("bash"));
        assert!(!debug.contains("/etc/passwd"));
        assert!(!debug.contains("raw_output"));
    }
}
