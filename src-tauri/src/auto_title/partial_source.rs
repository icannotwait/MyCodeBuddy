//! Batch partial assistant text for deadline promotion (multi-connection safe).
//!
//! Walks the connection map once per call, releases the map lock before any
//! `SessionState` read, and picks a deterministic winner when several
//! connections share a conversation id.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::acp::manager::ConnectionManager;

/// Injectable source of live partial assistant text for a set of conversations.
///
/// Used by the deadline sweep (Task 8) so tests can stub partials without a
/// real connection map. Production uses [`ManagerPartialSource`].
#[async_trait]
pub trait PartialAssistantTextSource: Send + Sync {
    /// Return raw visible partial text keyed by conversation id.
    ///
    /// Missing keys mean no matching live connection (promote treats as `""`).
    /// Present values are **unbounded** — the service applies `bound_context`.
    async fn partials_for(&self, conversation_ids: &[i32]) -> HashMap<i32, String>;
}

/// Production [`PartialAssistantTextSource`] backed by [`ConnectionManager`].
pub struct ManagerPartialSource {
    manager: ConnectionManager,
}

impl ManagerPartialSource {
    pub fn new(manager: ConnectionManager) -> Self {
        Self { manager }
    }

    /// Share the same connection map as `manager` via `clone_ref`.
    pub fn from_manager_ref(manager: &ConnectionManager) -> Self {
        Self {
            manager: manager.clone_ref(),
        }
    }
}

#[async_trait]
impl PartialAssistantTextSource for ManagerPartialSource {
    async fn partials_for(&self, conversation_ids: &[i32]) -> HashMap<i32, String> {
        self.manager
            .snapshot_partial_assistant_text_for_conversations(conversation_ids)
            .await
    }
}

/// Pure scoring input for multi-connection selection (testable without locks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartialCandidate {
    pub connection_id: String,
    pub has_live: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub text: String,
}

/// Whether `challenger` should replace `incumbent` under the multi-match rule:
/// prefer live, then max `started_at`, then connection id ascending.
pub(crate) fn is_better_partial_candidate(
    challenger: &PartialCandidate,
    incumbent: &PartialCandidate,
) -> bool {
    match (challenger.has_live, incumbent.has_live) {
        (true, false) => true,
        (false, true) => false,
        _ => match (challenger.started_at, incumbent.started_at) {
            (Some(c), Some(i)) if c != i => c > i,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            // Equal started_at (including both None): smaller connection id wins.
            _ => challenger.connection_id < incumbent.connection_id,
        },
    }
}

/// Pick the winning candidate for one conversation id (pure).
pub(crate) fn select_best_partial_candidate(
    candidates: &[PartialCandidate],
) -> Option<&PartialCandidate> {
    let mut best: Option<&PartialCandidate> = None;
    for c in candidates {
        match best {
            None => best = Some(c),
            Some(inc) if is_better_partial_candidate(c, inc) => best = Some(c),
            _ => {}
        }
    }
    best
}

/// Reduce scored candidates into `conversation_id → raw visible text`.
pub(crate) fn fold_partial_candidates(
    by_conversation: HashMap<i32, Vec<PartialCandidate>>,
) -> HashMap<i32, String> {
    by_conversation
        .into_iter()
        .filter_map(|(cid, candidates)| {
            select_best_partial_candidate(&candidates).map(|best| (cid, best.text.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use crate::acp::session_state::{LiveContentBlock, LiveMessage};
    use crate::models::message::MessageRole;
    use crate::web::event_bridge::EventEmitter;

    fn candidate(
        conn_id: &str,
        has_live: bool,
        started: Option<(i64, u32)>,
        text: &str,
    ) -> PartialCandidate {
        PartialCandidate {
            connection_id: conn_id.to_string(),
            has_live,
            started_at: started.map(|(secs, nsecs)| {
                Utc.timestamp_opt(secs, nsecs)
                    .single()
                    .expect("valid timestamp")
            }),
            text: text.to_string(),
        }
    }

    fn live(text: &str, started_secs: i64) -> LiveMessage {
        LiveMessage {
            id: format!("live-{started_secs}"),
            role: MessageRole::Assistant,
            content: vec![LiveContentBlock::Text {
                text: text.to_string(),
            }],
            started_at: Utc
                .timestamp_opt(started_secs, 0)
                .single()
                .expect("valid timestamp"),
        }
    }

    async fn insert_bound_with_live(
        mgr: &ConnectionManager,
        conn_id: &str,
        conversation_id: i32,
        live_message: Option<LiveMessage>,
    ) {
        mgr.insert_test_connection(
            conn_id,
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        let state = mgr
            .get_state(conn_id)
            .await
            .expect("test connection state");
        let mut guard = state.write().await;
        guard.conversation_id = Some(conversation_id);
        guard.live_message = live_message;
    }

    #[test]
    fn picks_newest_live_message_among_matches() {
        let older = candidate("conn-b", true, Some((1_700_000_000, 0)), "older text");
        let newer = candidate("conn-a", true, Some((1_700_000_100, 0)), "newer text");
        let no_live = candidate("conn-z", false, None, "");

        let pool = [older.clone(), newer.clone(), no_live.clone()];
        let best = select_best_partial_candidate(&pool).expect("winner");
        assert_eq!(best.connection_id, "conn-a");
        assert_eq!(best.text, "newer text");

        // Live always beats no-live even when the live text is empty and the
        // no-live candidate would otherwise sort first by id.
        let empty_live = candidate("conn-zz", true, Some((1_700_000_000, 0)), "");
        let pool = [no_live, empty_live.clone()];
        let best = select_best_partial_candidate(&pool).expect("winner");
        assert_eq!(best.connection_id, empty_live.connection_id);
        assert!(best.has_live);
    }

    #[test]
    fn equal_started_at_tie_breaks_by_connection_id_ascending() {
        let t = Some((1_700_000_050, 0));
        let high = candidate("conn-z", true, t, "from z");
        let mid = candidate("conn-m", true, t, "from m");
        let low = candidate("conn-a", true, t, "from a");

        let pool = [high, mid, low];
        let best = select_best_partial_candidate(&pool).expect("winner");
        assert_eq!(
            best.connection_id, "conn-a",
            "equal started_at → ascending connection id"
        );
        assert_eq!(best.text, "from a");
    }

    #[test]
    fn fold_partial_candidates_keeps_per_conversation_winners() {
        let mut by = HashMap::new();
        by.insert(
            1,
            vec![
                candidate("b", true, Some((10, 0)), "old"),
                candidate("a", true, Some((20, 0)), "new"),
            ],
        );
        by.insert(
            2,
            vec![
                candidate("x", true, Some((5, 0)), "same-t-x"),
                candidate("w", true, Some((5, 0)), "same-t-w"),
            ],
        );
        let map = fold_partial_candidates(by);
        assert_eq!(map.get(&1).map(String::as_str), Some("new"));
        assert_eq!(map.get(&2).map(String::as_str), Some("same-t-w"));
    }

    #[tokio::test]
    async fn snapshot_partial_does_not_use_find_by_conversation_id() {
        // Multi-match: two connections, same conversation_id, different
        // live_message.started_at → newer live text only. Implemented via a
        // single map walk (not per-id find_connection_by_conversation_id).
        let mgr = ConnectionManager::new();
        insert_bound_with_live(
            &mgr,
            "conn-old",
            42,
            Some(live("older partial", 1_700_000_000)),
        )
        .await;
        insert_bound_with_live(
            &mgr,
            "conn-new",
            42,
            Some(live("newer partial", 1_700_000_200)),
        )
        .await;
        // Unrelated conversation + unbound connection must not pollute results.
        insert_bound_with_live(
            &mgr,
            "conn-other",
            99,
            Some(live("other conv", 1_700_000_300)),
        )
        .await;
        mgr.insert_test_connection(
            "conn-unbound",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;

        let source = ManagerPartialSource::from_manager_ref(&mgr);
        let partials = source.partials_for(&[42, 7]).await;

        assert_eq!(
            partials.get(&42).map(String::as_str),
            Some("newer partial"),
            "must pick newest live among multi-match"
        );
        assert!(
            !partials.contains_key(&7),
            "missing conversation ids omitted (promote treats as empty)"
        );
        assert!(
            !partials.contains_key(&99),
            "ids not in the request must not appear"
        );

        // Equal started_at → connection id ascending through the manager path.
        let mgr2 = ConnectionManager::new();
        let t = 1_700_000_050;
        insert_bound_with_live(&mgr2, "conn-z", 7, Some(live("from z", t))).await;
        insert_bound_with_live(&mgr2, "conn-a", 7, Some(live("from a", t))).await;
        let tie = mgr2
            .snapshot_partial_assistant_text_for_conversations(&[7])
            .await;
        assert_eq!(tie.get(&7).map(String::as_str), Some("from a"));
    }

    #[tokio::test]
    async fn snapshot_partial_releases_map_lock_before_state_read() {
        // Hold a state write lock, start a concurrent snapshot that must
        // eventually read that state. If snapshot held the connections map
        // lock across state.read(), the map would stay locked while waiting
        // on the write guard — and this probe would time out.
        let mgr = ConnectionManager::new();
        insert_bound_with_live(
            &mgr,
            "conn-held",
            11,
            Some(live("held text", 1_700_000_000)),
        )
        .await;
        insert_bound_with_live(
            &mgr,
            "conn-free",
            22,
            Some(live("free text", 1_700_000_100)),
        )
        .await;

        let state_held = mgr
            .get_state("conn-held")
            .await
            .expect("held connection state");
        let write_guard = state_held.write().await;

        let mgr_snap = mgr.clone_ref();
        let snap_task = tokio::spawn(async move {
            mgr_snap
                .snapshot_partial_assistant_text_for_conversations(&[11, 22])
                .await
        });

        // Let the snapshot reach its first state.read (possibly blocked on
        // the write guard we still hold).
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let map_probe = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            mgr.connections.lock(),
        )
        .await;
        assert!(
            map_probe.is_ok(),
            "connections map must not stay locked across state.read"
        );
        drop(map_probe.unwrap());

        drop(write_guard);
        let partials = tokio::time::timeout(std::time::Duration::from_secs(2), snap_task)
            .await
            .expect("snapshot join timed out")
            .expect("snapshot task panicked");
        assert_eq!(partials.get(&11).map(String::as_str), Some("held text"));
        assert_eq!(partials.get(&22).map(String::as_str), Some("free text"));
    }

    #[tokio::test]
    async fn snapshot_partial_prefers_live_over_idle_same_conversation() {
        let mgr = ConnectionManager::new();
        insert_bound_with_live(&mgr, "idle", 5, None).await;
        insert_bound_with_live(&mgr, "live", 5, Some(live("streaming", 1_700_000_000))).await;

        let map = mgr
            .snapshot_partial_assistant_text_for_conversations(&[5])
            .await;
        assert_eq!(map.get(&5).map(String::as_str), Some("streaming"));
    }
}
