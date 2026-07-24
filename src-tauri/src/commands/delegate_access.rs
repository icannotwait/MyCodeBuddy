use crate::acp::manager::ConnectionManager;
use crate::db::entities::conversation::{ConversationKind, DelegationTaskStatus};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::{
    DbConversationSummary, DelegateAccessReason, DelegateAccessState,
};

fn unknown(parent_id: Option<i32>) -> DelegateAccessState {
    DelegateAccessState::viewer_only(DelegateAccessReason::StateUnknown, parent_id)
}

fn task_is_terminal(status: Option<&DelegationTaskStatus>) -> bool {
    matches!(
        status,
        Some(DelegationTaskStatus::Completed)
            | Some(DelegationTaskStatus::Failed)
            | Some(DelegationTaskStatus::Canceled)
    )
}

/// Multi-candidate parent live-turn probe.
///
/// Scans every live connection bound to the parent (conversation id and/or
/// external-id fallback). Any valid candidate with `turn_in_flight` locks.
/// Conflicting identity among candidates fails closed (`Err(())`).
async fn live_parent_turn(
    manager: &ConnectionManager,
    parent: &DbConversationSummary,
) -> Result<bool, ()> {
    let candidates = manager
        .find_all_connections_for_conversation_identity(
            parent.id,
            parent.external_id.as_deref(),
            parent.agent_type,
        )
        .await;
    if candidates.is_empty() {
        return Ok(false);
    }
    let mut saw_valid = false;
    let mut any_in_flight = false;
    for connection_id in candidates {
        let Some(state_arc) = manager.get_state(&connection_id).await else {
            return Err(());
        };
        let state = state_arc.read().await;
        if state.agent_type != parent.agent_type {
            return Err(());
        }
        let conv_ok = state.conversation_id == Some(parent.id)
            || (state.conversation_id.is_none()
                && parent.external_id.as_deref().is_some()
                && state.external_id.as_deref() == parent.external_id.as_deref());
        if !conv_ok {
            return Err(());
        }
        if let Some(expected) = parent.external_id.as_deref() {
            if state.external_id.as_deref().is_some()
                && state.external_id.as_deref() != Some(expected)
            {
                return Err(());
            }
        }
        saw_valid = true;
        if state.turn_in_flight {
            any_in_flight = true;
        }
    }
    if !saw_valid {
        return Err(());
    }
    Ok(any_in_flight)
}

/// Shared access projection for a conversation id.
///
/// Effective policy:
/// `viewer_only` when the child task is non-terminal, the immediate parent
/// durable status is `in_progress`, or any valid parent live connection has
/// `turn_in_flight`. Missing/contradictory identity fails closed as
/// `state_unknown`. Reason precedence: `task_running` > `parent_turn_active`
/// > `state_unknown`.
pub async fn get_delegate_access_core(
    db: &AppDatabase,
    manager: &ConnectionManager,
    conversation_id: i32,
) -> DelegateAccessState {
    let child = match conversation_service::get_by_id(&db.conn, conversation_id).await {
        Ok(child) => child,
        Err(_) => return unknown(None),
    };
    if child.kind != ConversationKind::Delegate {
        return DelegateAccessState::interactive(None);
    }
    let Some(parent_id) = child.parent_id else {
        return unknown(None);
    };
    let parent = match conversation_service::get_by_id(&db.conn, parent_id).await {
        Ok(parent) => parent,
        Err(_) => return unknown(Some(parent_id)),
    };
    if !task_is_terminal(child.delegation_task_status.as_ref()) {
        return DelegateAccessState::viewer_only(
            DelegateAccessReason::TaskRunning,
            Some(parent_id),
        );
    }
    let durable_active = match parent.status.as_str() {
        "in_progress" => true,
        "pending_review" | "completed" | "cancelled" => false,
        _ => return unknown(Some(parent_id)),
    };
    if durable_active {
        return DelegateAccessState::viewer_only(
            DelegateAccessReason::ParentTurnActive,
            Some(parent_id),
        );
    }
    match live_parent_turn(manager, &parent).await {
        Ok(true) => DelegateAccessState::viewer_only(
            DelegateAccessReason::ParentTurnActive,
            Some(parent_id),
        ),
        Ok(false) => DelegateAccessState::interactive(Some(parent_id)),
        Err(()) => unknown(Some(parent_id)),
    }
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn get_delegate_access(
    conversation_id: i32,
    db: tauri::State<'_, AppDatabase>,
    manager: tauri::State<'_, ConnectionManager>,
) -> Result<DelegateAccessState, crate::acp::error::AcpError> {
    Ok(get_delegate_access_core(&db, &manager, conversation_id).await)
}

#[cfg(test)]
mod tests {
    use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::entities::conversation::{self, ConversationStatus, DelegationTaskStatus};
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::{AgentType, DelegateAccessMode};
    use crate::web::event_bridge::EventEmitter;

    async fn fixture() -> (AppDatabase, ConnectionManager, i32, i32) {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/delegate-access").await;
        let parent = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tool-1".into(),
                delegation_call_id: "task-1".into(),
            }),
        )
        .await
        .unwrap();
        (db, ConnectionManager::new(), parent.id, child.id)
    }

    async fn set_parent_status(db: &AppDatabase, id: i32, status: ConversationStatus) {
        conversation_service::update_status(&db.conn, id, status)
            .await
            .unwrap();
    }

    async fn set_child_task(db: &AppDatabase, id: i32, status: Option<DelegationTaskStatus>) {
        let row = conversation::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.delegation_task_status = Set(status);
        active.update(&db.conn).await.unwrap();
    }

    #[tokio::test]
    async fn running_child_wins_reason_precedence() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_parent_status(&db, parent_id, ConversationStatus::InProgress).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await,
            DelegateAccessState::viewer_only(DelegateAccessReason::TaskRunning, Some(parent_id))
        );
    }

    #[tokio::test]
    async fn terminal_child_unlocks_only_after_parent_is_idle() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::ParentTurnActive)
        );
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id).await.mode,
            DelegateAccessMode::Interactive
        );
    }

    #[tokio::test]
    async fn live_parent_turn_relocks_before_durable_status_changes() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Failed)).await;
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        manager
            .insert_test_connection(
                "parent-live",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state("parent-live").await.unwrap();
        {
            let mut state = state.write().await;
            state.conversation_id = Some(parent_id);
            state.turn_in_flight = true;
        }
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::ParentTurnActive)
        );
    }

    #[tokio::test]
    async fn later_parent_turn_relocks_every_direct_terminal_child() {
        let (db, manager, parent_id, child_id) = fixture().await;
        let folder_id = conversation_service::get_by_id(&db.conn, child_id)
            .await
            .unwrap()
            .folder_id;
        let sibling = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("sibling".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tool-2".into(),
                delegation_call_id: "task-2".into(),
            }),
        )
        .await
        .unwrap();

        for id in [child_id, sibling.id] {
            set_child_task(&db, id, Some(DelegationTaskStatus::Completed)).await;
        }
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        for id in [child_id, sibling.id] {
            assert_eq!(
                get_delegate_access_core(&db, &manager, id).await.mode,
                DelegateAccessMode::Interactive
            );
        }

        set_parent_status(&db, parent_id, ConversationStatus::InProgress).await;
        for id in [child_id, sibling.id] {
            assert_eq!(
                get_delegate_access_core(&db, &manager, id).await.reason,
                Some(DelegateAccessReason::ParentTurnActive)
            );
        }
    }

    #[tokio::test]
    async fn conflicting_live_parent_identity_fails_closed() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        manager
            .insert_test_connection(
                "conflicting-parent",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state("conflicting-parent").await.unwrap();
        {
            let mut state = state.write().await;
            state.conversation_id = Some(parent_id);
            // Intentionally mismatch agent_type or external_id vs parent row
            // so identity validation returns Err → state_unknown.
            state.agent_type = AgentType::Gemini;
        }

        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::StateUnknown)
        );
    }

    #[tokio::test]
    async fn duplicate_valid_parent_candidates_lock_order_independent() {
        async fn run(order: &[(&str, bool)]) {
            let (db, manager, parent_id, child_id) = fixture().await;
            set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
            set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
            for (id, in_flight) in order {
                manager
                    .insert_test_connection(*id, AgentType::ClaudeCode, None, EventEmitter::Noop)
                    .await;
                let state = manager.get_state(*id).await.unwrap();
                let mut s = state.write().await;
                s.conversation_id = Some(parent_id);
                s.turn_in_flight = *in_flight;
            }
            assert_eq!(
                get_delegate_access_core(&db, &manager, child_id)
                    .await
                    .reason,
                Some(DelegateAccessReason::ParentTurnActive)
            );
        }
        // Both insertion orders: in_flight second, then first.
        run(&[("parent-a", false), ("parent-b", true)]).await;
        run(&[("parent-b", true), ("parent-a", false)]).await;
    }

    #[tokio::test]
    async fn external_id_only_in_flight_parent_candidate_locks() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, Some(DelegationTaskStatus::Completed)).await;
        set_parent_status(&db, parent_id, ConversationStatus::Completed).await;
        // create() leaves external_id None — seed it explicitly.
        conversation_service::update_external_id(&db.conn, parent_id, "parent-session".into())
            .await
            .unwrap();
        let parent = conversation_service::get_by_id(&db.conn, parent_id)
            .await
            .unwrap();
        let external = parent
            .external_id
            .clone()
            .expect("parent external_id seeded");
        manager
            .insert_test_connection("parent-ext", parent.agent_type, None, EventEmitter::Noop)
            .await;
        {
            let state = manager.get_state("parent-ext").await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = None;
            s.external_id = Some(external);
            s.agent_type = parent.agent_type;
            s.turn_in_flight = true;
        }
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::ParentTurnActive)
        );
    }

    #[tokio::test]
    async fn missing_task_and_parent_fail_closed() {
        let (db, manager, parent_id, child_id) = fixture().await;
        set_child_task(&db, child_id, None).await;
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::TaskRunning)
        );

        set_child_task(&db, child_id, Some(DelegationTaskStatus::Canceled)).await;
        conversation::Entity::delete_by_id(parent_id)
            .exec(&db.conn)
            .await
            .unwrap();
        assert_eq!(
            get_delegate_access_core(&db, &manager, child_id)
                .await
                .reason,
            Some(DelegateAccessReason::StateUnknown)
        );
    }
}
