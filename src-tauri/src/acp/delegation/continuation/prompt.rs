use serde::Serialize;

use super::store::{ContStoreError, ContinuationStore};
use super::types::ContinuationWakeReason;
use crate::acp::delegation::types::DelegationStatusBatch;
use crate::models::{ContentBlock, MessageTurn, TurnRole};

const INTERNAL_PROMPT_PREFIX: &str = "<!-- codeg-internal-continuation:";
const INTERNAL_PROMPT_SUFFIX: &str = " -->";

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct DelegationContinuationOrigin {
    continuation_id: String,
    generation: u64,
    wake_reason: ContinuationWakeReason,
    internal_prompt_id: String,
    internal_prompt_marker: String,
}

#[allow(dead_code)]
impl DelegationContinuationOrigin {
    pub(super) fn new(
        continuation_id: String,
        generation: u64,
        wake_reason: ContinuationWakeReason,
        internal_prompt_id: String,
        internal_prompt_marker: String,
    ) -> Self {
        Self {
            continuation_id,
            generation,
            wake_reason,
            internal_prompt_id,
            internal_prompt_marker,
        }
    }

    pub(crate) fn continuation_id(&self) -> &str {
        &self.continuation_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn internal_prompt_id(&self) -> &str {
        &self.internal_prompt_id
    }

    pub(crate) fn internal_prompt_marker(&self) -> &str {
        &self.internal_prompt_marker
    }

    pub(crate) fn wake_reason(&self) -> ContinuationWakeReason {
        self.wake_reason
    }
}

#[allow(dead_code)]
#[derive(Serialize)]
struct ContinuationPromptEnvelope<'a> {
    version: u8,
    continuation_id: &'a str,
    generation: u64,
    wake_reason: ContinuationWakeReason,
    snapshot: &'a DelegationStatusBatch,
}

#[allow(dead_code)]
pub(crate) fn internal_prompt_marker(continuation_id: &str, internal_prompt_id: &str) -> String {
    format!(
        "{INTERNAL_PROMPT_PREFIX}{continuation_id}:{internal_prompt_id}{INTERNAL_PROMPT_SUFFIX}"
    )
}

#[allow(dead_code)]
pub(crate) fn build_continuation_prompt_text(
    origin: &DelegationContinuationOrigin,
    snapshot: &DelegationStatusBatch,
) -> Result<String, serde_json::Error> {
    let envelope = ContinuationPromptEnvelope {
        version: 1,
        continuation_id: origin.continuation_id(),
        generation: origin.generation(),
        wake_reason: origin.wake_reason,
        snapshot,
    };
    Ok(format!(
        "{}\n{}",
        origin.internal_prompt_marker,
        serde_json::to_string(&envelope)?
    ))
}

pub(crate) async fn filter_internal_continuation_turns(
    store: &dyn ContinuationStore,
    conversation_id: i32,
    turns: &mut Vec<MessageTurn>,
) -> Result<(), ContStoreError> {
    let mut visible = Vec::with_capacity(turns.len());
    for turn in turns.drain(..) {
        let Some(marker) = marker_from_turn(&turn) else {
            visible.push(turn);
            continue;
        };
        if !store
            .matches_admitted_marker(conversation_id, marker)
            .await?
        {
            visible.push(turn);
        }
    }
    *turns = visible;
    Ok(())
}

fn marker_from_turn(turn: &MessageTurn) -> Option<&str> {
    if !matches!(turn.role, TurnRole::User) {
        return None;
    }
    let [ContentBlock::Text { text }] = turn.blocks.as_slice() else {
        return None;
    };
    let marker = text
        .split_once('\n')
        .map_or(text.as_str(), |(line, _)| line);
    is_internal_prompt_marker(marker).then_some(marker)
}

fn is_internal_prompt_marker(marker: &str) -> bool {
    let Some(body) = marker
        .strip_prefix(INTERNAL_PROMPT_PREFIX)
        .and_then(|value| value.strip_suffix(INTERNAL_PROMPT_SUFFIX))
    else {
        return false;
    };
    let Some((continuation_id, internal_prompt_id)) = body.split_once(':') else {
        return false;
    };
    !continuation_id.is_empty()
        && !internal_prompt_id.is_empty()
        && !internal_prompt_id.contains(':')
        && !body.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::acp::delegation::continuation::store::{
        ContinuationPatch, ContinuationStore, FieldPatch, InMemoryContinuationStore,
        NewContinuation,
    };
    use crate::acp::delegation::continuation::types::{
        ContinuationState, ContinuationTaskIds, ContinuationWakeReason,
    };
    use crate::acp::delegation::types::DelegationStatusBatch;
    use crate::models::{ContentBlock, MessageTurn, TurnRole};

    fn user_turn(id: &str, text: &str) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role: TurnRole::User,
            blocks: vec![ContentBlock::Text { text: text.into() }],
            timestamp: Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        }
    }

    fn assistant_turn(id: &str) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::Text { text: id.into() }],
            timestamp: Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        }
    }

    fn new_continuation(marker: String, conversation_id: i32) -> NewContinuation {
        let now = Utc::now();
        NewContinuation {
            continuation_id: "continuation-uuid".into(),
            parent_conversation_id: conversation_id,
            parent_session_id: "session".into(),
            parent_connection_id: "connection".into(),
            parent_turn_generation: 1,
            task_ids: ContinuationTaskIds(vec![]),
            armed_at: now,
            wake_at: now,
            internal_prompt_id: "prompt-uuid".into(),
            internal_prompt_marker: marker,
        }
    }

    async fn admit(store: &InMemoryContinuationStore, marker: String, conversation_id: i32) {
        let record = store
            .insert_arming(new_continuation(marker, conversation_id))
            .await
            .unwrap();
        let wake_pending = store
            .cas_transition(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                ContinuationPatch {
                    state: ContinuationState::WakePending,
                    wake_reason: FieldPatch::Keep,
                    suspend_requested_at: FieldPatch::Keep,
                    suspended_at: FieldPatch::Keep,
                    wake_claimed_at: FieldPatch::Keep,
                    prompt_admitted_at: FieldPatch::Keep,
                    finished_at: FieldPatch::Keep,
                    failure_code: FieldPatch::Keep,
                },
            )
            .await
            .unwrap()
            .unwrap();
        store
            .cas_transition(
                &wake_pending.continuation_id,
                wake_pending.generation,
                wake_pending.version,
                ContinuationState::WakePending,
                ContinuationPatch {
                    state: ContinuationState::Resuming,
                    wake_reason: FieldPatch::Keep,
                    suspend_requested_at: FieldPatch::Keep,
                    suspended_at: FieldPatch::Keep,
                    wake_claimed_at: FieldPatch::Keep,
                    prompt_admitted_at: FieldPatch::Set(Utc::now()),
                    finished_at: FieldPatch::Keep,
                    failure_code: FieldPatch::Keep,
                },
            )
            .await
            .unwrap();
    }

    #[test]
    fn continuation_prompt_embeds_exact_durable_marker_and_snapshot() {
        let origin = DelegationContinuationOrigin::new(
            "continuation-uuid".into(),
            7,
            ContinuationWakeReason::Checkpoint,
            "prompt-uuid".into(),
            internal_prompt_marker("continuation-uuid", "prompt-uuid"),
        );
        let snapshot = DelegationStatusBatch::legacy(vec![]);

        let text = build_continuation_prompt_text(&origin, &snapshot).unwrap();

        assert_eq!(
            text.lines().next(),
            Some("<!-- codeg-internal-continuation:continuation-uuid:prompt-uuid -->")
        );
        assert!(text.contains("\"version\":1"));
        assert!(text.contains("\"snapshot\":{\"tasks\":[]}"));
    }

    #[tokio::test]
    async fn continuation_filter_keeps_xml_looking_user_text() {
        let store = InMemoryContinuationStore::default();
        let mut turns = vec![user_turn(
            "user",
            "<!-- codeg-internal-continuation:continuation-uuid:prompt-uuid --> extra",
        )];

        filter_internal_continuation_turns(&store, 1, &mut turns)
            .await
            .unwrap();

        assert_eq!(turns.len(), 1);
    }

    #[tokio::test]
    async fn continuation_filter_keeps_unadmitted_marker() {
        let store = InMemoryContinuationStore::default();
        let marker = internal_prompt_marker("continuation-uuid", "prompt-uuid");
        store
            .insert_arming(new_continuation(marker.clone(), 1))
            .await
            .unwrap();
        let mut turns = vec![user_turn("user", &format!("{marker}\n{{}}"))];

        filter_internal_continuation_turns(&store, 1, &mut turns)
            .await
            .unwrap();

        assert_eq!(turns.len(), 1);
    }

    #[tokio::test]
    async fn continuation_filter_keeps_marker_from_other_conversation() {
        let store = InMemoryContinuationStore::default();
        let marker = internal_prompt_marker("continuation-uuid", "prompt-uuid");
        admit(&store, marker.clone(), 2).await;
        let mut turns = vec![user_turn("user", &format!("{marker}\n{{}}"))];

        filter_internal_continuation_turns(&store, 1, &mut turns)
            .await
            .unwrap();

        assert_eq!(turns.len(), 1);
    }

    #[tokio::test]
    async fn continuation_filter_removes_only_matching_admitted_user_turn() {
        let store = InMemoryContinuationStore::default();
        let marker = internal_prompt_marker("continuation-uuid", "prompt-uuid");
        admit(&store, marker.clone(), 1).await;
        let mut turns = vec![
            assistant_turn("before"),
            user_turn("internal", &format!("{marker}\n{{}}")),
            assistant_turn("after"),
        ];

        filter_internal_continuation_turns(&store, 1, &mut turns)
            .await
            .unwrap();

        assert_eq!(
            turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            vec!["before", "after"]
        );
    }
}
