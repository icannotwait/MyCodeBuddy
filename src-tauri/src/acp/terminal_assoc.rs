//! Conservative client-side fallback that links ACP client terminals to shell
//! tool calls when an agent creates a terminal but never sends
//! `ToolCallContent::Terminal { terminalId }`.
//!
//! Grok's `AcpTerminalAdapter` currently does `create → wait → output → release`
//! without associating the terminal to the tool call. DrawCode already polls
//! once that association exists; this module only synthesizes it when safe.
//!
//! Safety rule: bind only when there is exactly one unbound shell tool call
//! and exactly one unbound terminal in the session. Concurrent shell tools
//! (multi-subagent / parallel bash) refuse to guess.

use std::collections::HashMap;

/// Per-connection fallback state. Cheap to share behind `Mutex`.
#[derive(Debug, Default)]
pub struct TerminalAssocFallback {
    /// When false, all methods are no-ops (non-Grok agents).
    enabled: bool,
    sessions: HashMap<String, SessionAssoc>,
}

#[derive(Debug, Default)]
struct SessionAssoc {
    /// In-progress shell tool calls not yet linked to a terminal.
    candidates: Vec<String>,
    /// Terminals created without a unique tool association yet (FIFO).
    unbound_terminals: Vec<String>,
    /// Bindings the turn poller should merge into `tracked_terminal_tool_calls`.
    pending_binds: Vec<(String, String)>,
}

/// Snapshot of a tool_call / tool_call_update relevant to association.
#[derive(Debug, Clone)]
pub struct ToolCallAssocHint {
    pub tool_call_id: String,
    /// Lowercased `Debug` of ACP tool kind, if known (e.g. `"execute"`).
    pub kind: Option<String>,
    pub title: Option<String>,
    pub has_terminal_content: bool,
    /// Lowercased status (`"inprogress"`, `"completed"`, …).
    pub status: Option<String>,
}

impl TerminalAssocFallback {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            sessions: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Observe a shell-related tool call event. Updates candidates and may
    /// produce a unique bind when a previously unbound terminal is waiting.
    pub fn observe_tool(&mut self, session_id: &str, hint: ToolCallAssocHint) {
        if !self.enabled {
            return;
        }

        let session = self
            .sessions
            .entry(session_id.to_string())
            .or_default();

        let is_final = hint
            .status
            .as_deref()
            .is_some_and(is_final_tool_status);

        if hint.has_terminal_content || is_final {
            session
                .candidates
                .retain(|id| id != &hint.tool_call_id);
            // Official association or completion: no further guessing for this id.
            try_unique_bind(session);
            return;
        }

        if !is_shell_like_tool(hint.kind.as_deref(), hint.title.as_deref()) {
            // Unknown / non-shell tool: ignore unless already a candidate
            // (status-only updates without kind/title keep the candidate).
            if !session.candidates.iter().any(|id| id == &hint.tool_call_id) {
                return;
            }
        } else if !session.candidates.iter().any(|id| id == &hint.tool_call_id) {
            session.candidates.push(hint.tool_call_id.clone());
        }

        // Multiple concurrent shell tools → cannot attribute unbound terminals.
        if session.candidates.len() > 1 {
            session.unbound_terminals.clear();
            return;
        }

        try_unique_bind(session);
    }

    /// Called after a successful `terminal/create` for `session_id`.
    /// Returns the tool_call_id when a unique bind was recorded.
    pub fn on_terminal_created(
        &mut self,
        session_id: &str,
        terminal_id: &str,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let session = self
            .sessions
            .entry(session_id.to_string())
            .or_default();

        if session.candidates.len() > 1 {
            // Ambiguous — refuse to queue or bind.
            return None;
        }

        if !session
            .unbound_terminals
            .iter()
            .any(|id| id == terminal_id)
        {
            session
                .unbound_terminals
                .push(terminal_id.to_string());
        }

        try_unique_bind(session)
    }

    /// Drain bindings produced since the last drain for this session.
    pub fn drain_pending_binds(&mut self, session_id: &str) -> Vec<(String, String)> {
        if !self.enabled {
            return Vec::new();
        }
        self.sessions
            .get_mut(session_id)
            .map(|s| std::mem::take(&mut s.pending_binds))
            .unwrap_or_default()
    }

    pub fn clear_session(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
    }
}

fn try_unique_bind(session: &mut SessionAssoc) -> Option<String> {
    if session.candidates.len() != 1 || session.unbound_terminals.len() != 1 {
        return None;
    }

    let tool_call_id = session.candidates.remove(0);
    let terminal_id = session.unbound_terminals.remove(0);
    session
        .pending_binds
        .push((tool_call_id.clone(), terminal_id));
    Some(tool_call_id)
}

fn is_final_tool_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

/// Heuristic: treat execute-kind tools and common shell titles as candidates.
pub fn is_shell_like_tool(kind: Option<&str>, title: Option<&str>) -> bool {
    if let Some(kind) = kind {
        let k = kind.to_ascii_lowercase();
        // sacp Debug of ToolKind::Execute is typically "Execute" → "execute"
        if k == "execute" || k.contains("execute") {
            return true;
        }
    }

    let title = title.unwrap_or("").to_ascii_lowercase();
    if title.is_empty() {
        return false;
    }

    title.contains("run_terminal")
        || title.contains("run terminal")
        || title.contains("bash")
        || title.contains("shell")
        || title.contains("run_command")
        || title.contains("command")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_hint(id: &str, status: &str) -> ToolCallAssocHint {
        ToolCallAssocHint {
            tool_call_id: id.to_string(),
            kind: Some("execute".into()),
            title: Some("run_terminal_command".into()),
            has_terminal_content: false,
            status: Some(status.into()),
        }
    }

    fn read_hint(id: &str) -> ToolCallAssocHint {
        ToolCallAssocHint {
            tool_call_id: id.to_string(),
            kind: Some("read".into()),
            title: Some("read_file".into()),
            has_terminal_content: false,
            status: Some("inprogress".into()),
        }
    }

    #[test]
    fn disabled_fallback_never_binds() {
        let mut fb = TerminalAssocFallback::new(false);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
        assert!(fb.drain_pending_binds("s1").is_empty());
    }

    #[test]
    fn unique_shell_then_create_binds() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        assert_eq!(
            fb.on_terminal_created("s1", "term_a").as_deref(),
            Some("t1")
        );
        assert_eq!(
            fb.drain_pending_binds("s1"),
            vec![("t1".into(), "term_a".into())]
        );
        // Second drain is empty.
        assert!(fb.drain_pending_binds("s1").is_empty());
    }

    #[test]
    fn create_before_tool_then_unique_tool_binds() {
        let mut fb = TerminalAssocFallback::new(true);
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        assert_eq!(
            fb.drain_pending_binds("s1"),
            vec![("t1".into(), "term_a".into())]
        );
    }

    #[test]
    fn two_concurrent_shells_refuse_to_bind() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        fb.observe_tool("s1", shell_hint("t2", "inprogress"));
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
        assert!(fb.on_terminal_created("s1", "term_b").is_none());
        assert!(fb.drain_pending_binds("s1").is_empty());
    }

    #[test]
    fn after_one_shell_completes_next_unique_shell_can_bind() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        fb.observe_tool("s1", shell_hint("t2", "inprogress"));
        // concurrent — no bind
        assert!(fb.on_terminal_created("s1", "term_x").is_none());

        fb.observe_tool("s1", shell_hint("t1", "completed"));
        // t2 still alone; terminal was discarded while ambiguous, so create again
        assert_eq!(
            fb.on_terminal_created("s1", "term_y").as_deref(),
            Some("t2")
        );
        assert_eq!(
            fb.drain_pending_binds("s1"),
            vec![("t2".into(), "term_y".into())]
        );
    }

    #[test]
    fn official_terminal_content_drops_candidate() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        let mut with_term = shell_hint("t1", "inprogress");
        with_term.has_terminal_content = true;
        fb.observe_tool("s1", with_term);
        // candidate gone — create does not bind
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
        // terminal queued alone but no candidate
        assert!(fb.drain_pending_binds("s1").is_empty());
    }

    #[test]
    fn non_shell_tools_do_not_become_candidates() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", read_hint("r1"));
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
        assert!(fb.drain_pending_binds("s1").is_empty());
    }

    #[test]
    fn sessions_are_isolated() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        fb.observe_tool("s2", shell_hint("t2", "inprogress"));
        assert_eq!(
            fb.on_terminal_created("s1", "term_a").as_deref(),
            Some("t1")
        );
        assert_eq!(
            fb.on_terminal_created("s2", "term_b").as_deref(),
            Some("t2")
        );
        assert_eq!(
            fb.drain_pending_binds("s1"),
            vec![("t1".into(), "term_a".into())]
        );
        assert_eq!(
            fb.drain_pending_binds("s2"),
            vec![("t2".into(), "term_b".into())]
        );
    }

    #[test]
    fn is_shell_like_recognizes_execute_and_titles() {
        assert!(is_shell_like_tool(Some("execute"), None));
        assert!(is_shell_like_tool(Some("Execute"), Some("whatever")));
        assert!(is_shell_like_tool(
            None,
            Some("run_terminal_command")
        ));
        assert!(!is_shell_like_tool(Some("read"), Some("read_file")));
        assert!(!is_shell_like_tool(None, None));
    }

    #[test]
    fn clear_session_drops_state() {
        let mut fb = TerminalAssocFallback::new(true);
        fb.observe_tool("s1", shell_hint("t1", "inprogress"));
        fb.clear_session("s1");
        assert!(fb.on_terminal_created("s1", "term_a").is_none());
    }
}
