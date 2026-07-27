//! Grok private `_x.ai/*` session notifications → high-level actions.
//!
//! See design: `docs/superpowers/specs/2026-07-14-grok-compact-slash-acp-surfacing-design.md`

use sacp::UntypedMessage;

/// Call-site policy for private extension emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateExtEmitMode {
    /// In-prompt + pre-finalization drain: ContentDelta + UsageUpdate.
    InPrompt,
    /// Live idle loop: UsageUpdate only for completed compact.
    IdleUsageOnly,
    /// Historical session/load drain: drop compact payloads entirely.
    LoadDrainNoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XaiSessionAction {
    AgentText(String),
    Usage { used: u64 },
}

const XAI_PRIVATE_METHODS: &[&str] = &["_x.ai/session_notification", "_x.ai/session/update"];

/// Prefix a leading newline on 2nd+ compact lifecycle strings in a turn.
pub fn with_lifecycle_separator(mut text: String, compact_text_emitted_this_turn: bool) -> String {
    if compact_text_emitted_this_turn && !text.starts_with('\n') {
        text.insert(0, '\n');
    }
    text
}

/// English Codex-style completed compact line (no summary_preview in PR1).
pub fn format_compact_completed(before: Option<u64>, after: Option<u64>) -> String {
    match (before, after) {
        (Some(b), Some(a)) if b == a => {
            format!("Context compacted: {} tokens (no reduction).", group_u64(a))
        }
        (Some(b), Some(a)) => {
            format!(
                "Context compacted: {} → {} tokens.",
                group_u64(b),
                group_u64(a)
            )
        }
        (_, Some(a)) => format!("Context compacted: {} tokens remaining.", group_u64(a)),
        _ => "Context compacted.".into(),
    }
}

fn group_u64(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

fn json_u64(update: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(v) = update.get(*k) {
            if let Some(u) = v.as_u64() {
                return Some(u);
            }
            if let Some(i) = v.as_i64() {
                if i >= 0 {
                    return Some(i as u64);
                }
            }
        }
    }
    None
}

/// Pure mapper: private Grok notification → zero or more high-level actions.
///
/// Ignores `summary_preview` in PR1 (confidentiality / unclear contract).
pub fn map_xai_session_notification(
    notification: &UntypedMessage,
) -> Option<Vec<XaiSessionAction>> {
    let method = notification.method();
    if !XAI_PRIVATE_METHODS.contains(&method) {
        return None;
    }
    let Some(update) = notification.params().get("update") else {
        tracing::warn!("x.ai private notification missing params.update");
        return None;
    };
    let Some(kind) = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
    else {
        tracing::warn!("x.ai private notification missing sessionUpdate");
        return None;
    };
    if !kind.starts_with("auto_compact_") {
        return None;
    }
    match kind {
        "auto_compact_started" => Some(vec![XaiSessionAction::AgentText(
            "Compacting context…".into(),
        )]),
        "auto_compact_completed" => {
            let before = json_u64(update, &["tokens_before", "tokensBefore"]);
            let after = json_u64(update, &["tokens_after", "tokensAfter"]);
            let mut actions = vec![XaiSessionAction::AgentText(format_compact_completed(
                before, after,
            ))];
            if let Some(used) = after.filter(|&u| u > 0) {
                actions.push(XaiSessionAction::Usage { used });
            }
            Some(actions)
        }
        "auto_compact_failed" => Some(vec![XaiSessionAction::AgentText(
            "Context compaction failed.".into(),
        )]),
        "auto_compact_cancelled" => Some(vec![XaiSessionAction::AgentText(
            "Context compaction cancelled.".into(),
        )]),
        other => {
            tracing::debug!(
                session_update = other,
                "unhandled x.ai private compact kind"
            );
            None
        }
    }
}

/// Pure context-window size resolution (SessionState wrapper lives in connection).
///
/// Preference: user-configured `[model.<id>].context_window` → existing live
/// size → model-family inference → 256K default.
pub fn resolve_context_window_size_from_parts(
    existing_usage_size: Option<u64>,
    model_id: Option<&str>,
    configured_size: Option<u64>,
) -> u64 {
    if let Some(size) = configured_size.filter(|s| *s > 0) {
        return size;
    }
    if let Some(size) = existing_usage_size.filter(|s| *s > 0) {
        return size;
    }
    crate::parsers::infer_context_window_max_tokens(model_id.or(Some("grok"))).unwrap_or(256_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sacp::UntypedMessage;

    fn notif(method: &str, update: serde_json::Value) -> UntypedMessage {
        UntypedMessage::new(
            method,
            serde_json::json!({ "sessionId": "s", "update": update }),
        )
        .unwrap()
    }

    #[test]
    fn completed_maps_text_and_usage_snake_case() {
        let n = notif(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionUpdate": "auto_compact_completed",
                "tokens_before": 18592,
                "tokens_after": 18060,
                "summary_preview": "SHOULD_BE_IGNORED"
            }),
        );
        let actions = map_xai_session_notification(&n).expect("map");
        assert!(matches!(
            &actions[0],
            XaiSessionAction::AgentText(t)
                if t.contains("18,592")
                    && t.contains("18,060")
                    && !t.contains("SHOULD_BE_IGNORED")
        ));
        assert!(matches!(
            actions
                .iter()
                .find(|a| matches!(a, XaiSessionAction::Usage { .. })),
            Some(XaiSessionAction::Usage { used: 18060 })
        ));
    }

    #[test]
    fn dual_methods_accept_compact() {
        for method in ["_x.ai/session_notification", "_x.ai/session/update"] {
            let n = notif(
                method,
                serde_json::json!({ "sessionUpdate": "auto_compact_started" }),
            );
            assert!(map_xai_session_notification(&n).is_some(), "{method}");
        }
    }

    #[test]
    fn wrong_method_none() {
        let n = notif(
            "_claude/sdkMessage",
            serde_json::json!({ "sessionUpdate": "auto_compact_completed", "tokens_after": 1 }),
        );
        assert!(map_xai_session_notification(&n).is_none());
    }

    #[test]
    fn started_failed_cancelled_text_only() {
        for (kind, needle) in [
            ("auto_compact_started", "Compacting"),
            ("auto_compact_failed", "failed"),
            ("auto_compact_cancelled", "cancelled"),
        ] {
            let n = notif(
                "_x.ai/session_notification",
                serde_json::json!({
                    "sessionUpdate": kind,
                    "tokens_after": 99
                }),
            );
            let actions = map_xai_session_notification(&n).unwrap();
            assert_eq!(actions.len(), 1, "{kind}");
            assert!(matches!(&actions[0], XaiSessionAction::AgentText(t) if t.contains(needle)));
        }
    }

    #[test]
    fn tokens_after_zero_skips_usage() {
        let n = notif(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionUpdate": "auto_compact_completed",
                "tokens_before": 10,
                "tokens_after": 0
            }),
        );
        let actions = map_xai_session_notification(&n).unwrap();
        assert!(actions
            .iter()
            .all(|a| !matches!(a, XaiSessionAction::Usage { .. })));
    }

    #[test]
    fn before_equals_after_wording() {
        let t = format_compact_completed(Some(1024), Some(1024));
        assert!(t.contains("no reduction"));
    }

    #[test]
    fn camel_case_token_fields() {
        let n = notif(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionUpdate": "auto_compact_completed",
                "tokensBefore": 100,
                "tokensAfter": 90
            }),
        );
        let actions = map_xai_session_notification(&n).unwrap();
        assert!(matches!(
            actions
                .iter()
                .find(|a| matches!(a, XaiSessionAction::Usage { .. })),
            Some(XaiSessionAction::Usage { used: 90 })
        ));
    }

    #[test]
    fn lifecycle_separator_prefixes_second() {
        let first = with_lifecycle_separator("Compacting context…".into(), false);
        assert!(!first.starts_with('\n'));
        let second = with_lifecycle_separator("Context compacted.".into(), true);
        assert!(second.starts_with('\n'));
    }

    #[test]
    fn fixture_file_maps_completed() {
        let raw = include_str!("fixtures/grok_auto_compact_completed.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let method = v["method"].as_str().unwrap();
        let n = UntypedMessage::new(method, v["params"].clone()).unwrap();
        let actions = map_xai_session_notification(&n).expect("fixture");
        assert!(matches!(&actions[0], XaiSessionAction::AgentText(_)));
    }

    #[test]
    fn failed_exact_text_no_usage() {
        let n = notif(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionUpdate": "auto_compact_failed",
                "tokens_after": 50,
                "reason": "ignored"
            }),
        );
        let actions = map_xai_session_notification(&n).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            XaiSessionAction::AgentText(t) if t == "Context compaction failed."
        ));
    }

    #[test]
    fn after_only_wording() {
        assert_eq!(
            format_compact_completed(None, Some(18060)),
            "Context compacted: 18,060 tokens remaining."
        );
    }

    #[test]
    fn allowed_method_non_compact_returns_none() {
        let n = notif(
            "_x.ai/session/update",
            serde_json::json!({ "sessionUpdate": "turn_completed", "stop_reason": "end_turn" }),
        );
        assert!(map_xai_session_notification(&n).is_none());
    }

    #[test]
    fn resolve_prefers_configured_over_existing_and_model() {
        assert_eq!(
            resolve_context_window_size_from_parts(Some(500_000), Some("grok-4.5"), Some(131_072)),
            131_072
        );
    }

    #[test]
    fn resolve_prefers_existing_size_when_no_config() {
        assert_eq!(
            resolve_context_window_size_from_parts(Some(500_000), Some("grok-4.5"), None),
            500_000
        );
    }

    #[test]
    fn resolve_uses_model_family_when_no_usage_size() {
        assert_eq!(
            resolve_context_window_size_from_parts(None, Some("grok-4.5"), None),
            500_000
        );
        assert_eq!(
            resolve_context_window_size_from_parts(None, Some("grok-4.3"), None),
            1_000_000
        );
        assert_eq!(
            resolve_context_window_size_from_parts(None, Some("grok-code-fast-1"), None),
            256_000
        );
        assert_eq!(
            resolve_context_window_size_from_parts(None, Some("grok-4-fast"), None),
            2_000_000
        );
    }

    #[test]
    fn resolve_defaults_unknown_grok() {
        assert_eq!(
            resolve_context_window_size_from_parts(None, None, None),
            256_000
        );
    }
}
