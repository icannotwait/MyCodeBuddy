//! Conversation history windowing: return only the tail (or a page before a
//! cursor) of `MessageTurn`s measured in **user turns**.
//!
//! Profiling of long Codex sessions (e.g. 30MB / ~6900 fine-grained turns)
//! showed cold open cost is dominated by shipping the full detail JSON and
//! adapting every turn in the frontend. Virtualized rendering already limits
//! DOM; this module shrinks the IPC + store payload.

use crate::models::{HistoryWindowInfo, MessageTurn, TurnRole};

/// Default number of user turns returned on cold open / refetch when the
/// client opts into windowing without an explicit limit.
pub const DEFAULT_HISTORY_USER_TURN_LIMIT: u32 = 20;

/// Optional window request attached to `get_folder_conversation`.
#[derive(Debug, Clone, Default)]
pub struct HistoryLoadOpts {
    /// `None` → no windowing (full history, backward compatible).
    /// `Some(0)` → no windowing (explicit unlimited).
    /// `Some(n)` where n > 0 → at most `n` user turns (plus intervening
    /// assistant/system turns) from the end of the eligible range.
    pub user_turn_limit: Option<u32>,
    /// Exclusive upper bound: only consider turns strictly before this turn
    /// id (used for "load older" pages). A missing/stale id returns an empty
    /// page rather than returning a non-older tail under a false cursor.
    pub before_turn_id: Option<String>,
}

#[derive(Debug)]
pub struct WindowedTurns {
    pub turns: Vec<MessageTurn>,
    pub window: HistoryWindowInfo,
}

/// Apply a user-turn window to a fully-parsed turn list.
///
/// When `opts.user_turn_limit` is `None` or `Some(0)`, returns all eligible
/// turns (still honoring `before_turn_id` if set) and reports
/// `has_more_before` relative to that range.
pub fn window_message_turns(mut turns: Vec<MessageTurn>, opts: &HistoryLoadOpts) -> WindowedTurns {
    let total_turn_count = turns.len() as u32;
    let total_user_turn_count = turns
        .iter()
        .filter(|t| matches!(t.role, TurnRole::User))
        .count() as u32;

    // Restrict upper bound for "load older".
    if let Some(before_id) = opts.before_turn_id.as_deref() {
        if let Some(idx) = turns.iter().position(|t| t.id == before_id) {
            turns.truncate(idx);
        } else {
            turns.clear();
        }
    }

    let limit = opts.user_turn_limit.unwrap_or(0);
    if limit == 0 {
        // Unlimited: the returned set is the full eligible range (possibly
        // truncated by before_turn_id), so nothing is "more before".
        let returned_user = turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::User))
            .count() as u32;
        return WindowedTurns {
            window: HistoryWindowInfo {
                has_more_before: false,
                total_turn_count,
                total_user_turn_count,
                user_turn_limit: 0,
                returned_user_turn_count: returned_user,
            },
            turns,
        };
    }
    if turns.is_empty() {
        return WindowedTurns {
            window: HistoryWindowInfo {
                has_more_before: false,
                total_turn_count,
                total_user_turn_count,
                user_turn_limit: limit,
                returned_user_turn_count: 0,
            },
            turns,
        };
    }

    // Walk from the end counting user turns until we hit `limit`.
    let mut user_seen = 0u32;
    let mut start = 0usize;
    let mut found = false;
    for i in (0..turns.len()).rev() {
        if matches!(turns[i].role, TurnRole::User) {
            user_seen += 1;
            if user_seen >= limit {
                start = i;
                found = true;
                break;
            }
        }
    }
    if !found {
        start = 0;
    }

    let has_more_before = start > 0;
    if start > 0 {
        turns = turns.split_off(start);
    }

    let returned_user = turns
        .iter()
        .filter(|t| matches!(t.role, TurnRole::User))
        .count() as u32;

    WindowedTurns {
        window: HistoryWindowInfo {
            has_more_before,
            total_turn_count,
            total_user_turn_count,
            user_turn_limit: limit,
            returned_user_turn_count: returned_user,
        },
        turns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TurnRole;
    use chrono::Utc;

    fn turn(id: &str, role: TurnRole) -> MessageTurn {
        MessageTurn {
            id: id.to_string(),
            role,
            blocks: vec![],
            timestamp: Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            reasoning_effort: None,
            completed_at: None,
            outcome: None,
        }
    }

    /// Build U A A U A U A A A (3 user turns, many assistant).
    fn sample() -> Vec<MessageTurn> {
        vec![
            turn("u1", TurnRole::User),
            turn("a1", TurnRole::Assistant),
            turn("a2", TurnRole::Assistant),
            turn("u2", TurnRole::User),
            turn("a3", TurnRole::Assistant),
            turn("u3", TurnRole::User),
            turn("a4", TurnRole::Assistant),
            turn("a5", TurnRole::Assistant),
            turn("a6", TurnRole::Assistant),
        ]
    }

    #[test]
    fn unlimited_returns_all() {
        let w = window_message_turns(sample(), &HistoryLoadOpts::default());
        assert_eq!(w.turns.len(), 9);
        assert!(!w.window.has_more_before);
        assert_eq!(w.window.total_turn_count, 9);
        assert_eq!(w.window.total_user_turn_count, 3);
        assert_eq!(w.window.user_turn_limit, 0);
        assert_eq!(w.window.returned_user_turn_count, 3);
    }

    #[test]
    fn tail_one_user_turn_includes_following_assistants() {
        let w = window_message_turns(
            sample(),
            &HistoryLoadOpts {
                user_turn_limit: Some(1),
                before_turn_id: None,
            },
        );
        let ids: Vec<_> = w.turns.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u3", "a4", "a5", "a6"]);
        assert!(w.window.has_more_before);
        assert_eq!(w.window.returned_user_turn_count, 1);
    }

    #[test]
    fn tail_two_user_turns() {
        let w = window_message_turns(
            sample(),
            &HistoryLoadOpts {
                user_turn_limit: Some(2),
                before_turn_id: None,
            },
        );
        let ids: Vec<_> = w.turns.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u2", "a3", "u3", "a4", "a5", "a6"]);
        assert!(w.window.has_more_before);
        assert_eq!(w.window.returned_user_turn_count, 2);
    }

    #[test]
    fn load_older_before_cursor() {
        // First page: last 1 user turn → u3..
        // Load older before u3 with limit 1 → u2 + a3
        let w = window_message_turns(
            sample(),
            &HistoryLoadOpts {
                user_turn_limit: Some(1),
                before_turn_id: Some("u3".into()),
            },
        );
        let ids: Vec<_> = w.turns.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u2", "a3"]);
        assert!(w.window.has_more_before);
    }

    #[test]
    fn load_older_reaches_start() {
        let w = window_message_turns(
            sample(),
            &HistoryLoadOpts {
                user_turn_limit: Some(10),
                before_turn_id: Some("u2".into()),
            },
        );
        let ids: Vec<_> = w.turns.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "a1", "a2"]);
        assert!(!w.window.has_more_before);
    }

    #[test]
    fn missing_before_id_returns_no_page() {
        let w = window_message_turns(
            sample(),
            &HistoryLoadOpts {
                user_turn_limit: Some(1),
                before_turn_id: Some("missing".into()),
            },
        );
        let ids: Vec<_> = w.turns.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.is_empty());
        assert!(!w.window.has_more_before);
        assert_eq!(w.window.user_turn_limit, 1);
        assert_eq!(w.window.returned_user_turn_count, 0);
    }

    #[test]
    fn empty_input() {
        let w = window_message_turns(
            vec![],
            &HistoryLoadOpts {
                user_turn_limit: Some(20),
                before_turn_id: None,
            },
        );
        assert!(w.turns.is_empty());
        assert!(!w.window.has_more_before);
        assert_eq!(w.window.total_turn_count, 0);
        assert_eq!(w.window.user_turn_limit, 20);
    }
}
