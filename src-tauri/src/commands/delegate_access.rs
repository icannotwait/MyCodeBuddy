use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::acp::manager::ConnectionManager;
use crate::auto_title::ConnectionPurpose;
use crate::db::entities::conversation::{self, ConversationKind, DelegationTaskStatus};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::{
    AgentType, DbConversationSummary, DelegateAccessMode, DelegateAccessReason,
    DelegateAccessState,
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

pub async fn ensure_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    conversation_id: i32,
) -> Result<(), crate::acp::error::AcpError> {
    let access = get_delegate_access_core(db, manager, conversation_id).await;
    if access.mode == DelegateAccessMode::Interactive {
        return Ok(());
    }
    Err(crate::acp::error::AcpError::DelegateViewerOnly {
        reason: access
            .reason
            .unwrap_or(DelegateAccessReason::StateUnknown),
    })
}

/// Resolve durable conversation id by external session identity.
/// - Ok(None): no matching row
/// - Ok(Some(id)): exactly one row for (external_id, agent_type)
/// - Err(DelegateViewerOnly{state_unknown}): multiple ambiguous rows
async fn resolve_conversation_id_from_external(
    db: &AppDatabase,
    external_id: Option<&str>,
    agent_type: AgentType,
) -> Result<Option<i32>, crate::acp::error::AcpError> {
    let Some(external_id) = external_id.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let at_str = serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let matches = conversation::Entity::find()
        .filter(conversation::Column::ExternalId.eq(external_id))
        .filter(conversation::Column::AgentType.eq(at_str))
        .filter(conversation::Column::DeletedAt.is_null())
        .all(&db.conn)
        .await
        .map_err(|_| crate::acp::error::AcpError::DelegateViewerOnly {
            reason: DelegateAccessReason::StateUnknown,
        })?;
    match matches.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(row.id)),
        _ => Err(crate::acp::error::AcpError::DelegateViewerOnly {
            reason: DelegateAccessReason::StateUnknown,
        }),
    }
}

/// Prefer this helper when the caller may supply an explicit conversation id
/// that is not yet bound on SessionState (prompt/fork first-link paths).
pub async fn ensure_effective_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    connection_id: &str,
    request_conversation_id: Option<i32>,
) -> Result<(), crate::acp::error::AcpError> {
    let state = manager.get_state(connection_id).await.ok_or_else(|| {
        crate::acp::error::AcpError::ConnectionNotFound(connection_id.to_string())
    })?;
    let (state_conv, external_id, agent_type, purpose) = {
        let s = state.read().await;
        (
            s.conversation_id,
            s.external_id.clone(),
            s.agent_type,
            s.purpose,
        )
    };
    // Always resolve external identity when present and cross-check every
    // non-None source. Never prefer request conversation_id alone when state
    // external_id points at a different durable row (Amendment 19).
    let from_external =
        resolve_conversation_id_from_external(db, external_id.as_deref(), agent_type).await?;
    let sources = [request_conversation_id, state_conv, from_external];
    let mut effective: Option<i32> = None;
    for candidate in sources.into_iter().flatten() {
        match effective {
            None => effective = Some(candidate),
            Some(existing) if existing == candidate => {}
            Some(_) => {
                return Err(crate::acp::error::AcpError::DelegateViewerOnly {
                    reason: DelegateAccessReason::StateUnknown,
                });
            }
        }
    }
    match effective {
        Some(id) => ensure_delegate_interactive(db, manager, id).await,
        // Brand-new *user* root: no durable id on request/state/session.
        // Broker-spawned delegation children can race before conversation_id /
        // external_id bind (reserved delegate row pending adoption). Treating
        // that as a root would let a user prompt create a regular row and
        // corrupt identity — fail closed until effective identity resolves.
        None if purpose == ConnectionPurpose::Delegation => {
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })
        }
        None => Ok(()),
    }
}

pub async fn ensure_connection_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    connection_id: &str,
) -> Result<(), crate::acp::error::AcpError> {
    ensure_effective_delegate_interactive(db, manager, connection_id, None).await
}

/// Admission for `acp_answer_question`.
///
/// `ConnectionManager::answer_question` routes by `question_id` and ignores the
/// caller-supplied `connection_id`. Guarding only the caller id is therefore
/// bypassable: pass any interactive connection + a locked-delegate question id.
/// Resolve the authoritative owner of the pending question and enforce viewer-
/// only on **that** connection. Missing/already-resolved ids leave the no-op
/// answer path unblocked (idempotent success).
pub async fn ensure_pending_question_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    question_id: &str,
) -> Result<(), crate::acp::error::AcpError> {
    if let Some(owner_connection_id) = manager
        .pending_question_parent_connection_id(question_id)
        .await
    {
        ensure_connection_delegate_interactive(db, manager, &owner_connection_id).await?;
    }
    Ok(())
}

/// Resolve connect-time conversation target before preflight/spawn.
/// Agreement rules:
/// - If request conversation_id is Some, load that row and (when session_id is
///   also Some) require external_id/agent_type agreement.
/// - If request conversation_id is None but session_id is Some, load the durable
///   row by (external_id=session_id, agent_type) and use that id when found.
/// - If both resolve and disagree → DelegateViewerOnly { state_unknown }.
/// - If the effective row is a locked delegate → ensure_delegate_interactive.
pub async fn ensure_connect_delegate_interactive(
    db: &AppDatabase,
    manager: &ConnectionManager,
    agent_type: AgentType,
    session_id: Option<&str>,
    conversation_id: Option<i32>,
) -> Result<(), crate::acp::error::AcpError> {
    let from_session = match session_id {
        Some(sid) if !sid.is_empty() => {
            resolve_conversation_id_from_external(db, Some(sid), agent_type).await?
        }
        _ => None,
    };
    let effective = match (conversation_id, from_session) {
        (Some(req), Some(found)) if req != found => {
            return Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            });
        }
        (Some(req), _) => Some(req),
        (None, Some(found)) => Some(found),
        (None, None) => None,
    };
    if let Some(id) = effective {
        ensure_delegate_interactive(db, manager, id).await?;
    }
    Ok(())
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
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
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

    #[tokio::test]
    async fn connection_guard_rejects_locked_delegate_and_accepts_regular() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        manager
            .insert_test_connection("child-live", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        manager
            .get_state("child-live")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(child_id);
        assert!(matches!(
            ensure_connection_delegate_interactive(&db, &manager, "child-live").await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::TaskRunning,
            })
        ));

        let folder = seed_folder(&db, "/tmp/delegate-access-regular").await;
        let regular = conversation_service::create(
            &db.conn,
            folder,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        manager
            .get_state("child-live")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(regular.id);
        ensure_connection_delegate_interactive(&db, &manager, "child-live")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn effective_guard_rejects_unbound_connection_with_locked_request_id() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        manager
            .insert_test_connection("unbound", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        // conversation_id stays None — mutation supplies locked child id.
        assert!(matches!(
            ensure_effective_delegate_interactive(
                &db,
                &manager,
                "unbound",
                Some(child_id),
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::TaskRunning,
            })
        ));
    }

    /// Broker pre-creates a delegate row then spawns child ACP. Early SessionState
    /// may have neither conversation_id nor external_id. That must NOT be treated
    /// as a brand-new root (which would allow prompt to create a regular row).
    #[tokio::test]
    async fn effective_guard_fails_closed_for_unbound_delegation_bootstrap() {
        use crate::auto_title::ConnectionPurpose;
        use crate::db::entities::conversation;

        let (db, manager, _parent_id, child_id) = fixture().await;
        // Reserved delegate exists, but connection is still unbound.
        let before = conversation::Entity::find()
            .all(&db.conn)
            .await
            .unwrap()
            .len();

        manager
            .insert_test_connection(
                "broker-bootstrap",
                AgentType::Codex,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let state = manager.get_state("broker-bootstrap").await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = None;
            s.external_id = None;
            s.purpose = ConnectionPurpose::Delegation;
        }

        assert!(matches!(
            ensure_effective_delegate_interactive(
                &db,
                &manager,
                "broker-bootstrap",
                None,
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })
        ));

        // Guard only — must not create or mutate durable conversation rows.
        let after = conversation::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(after.len(), before, "bootstrap reject must not insert rows");
        assert!(
            after.iter().any(|row| row.id == child_id),
            "reserved delegate row must remain"
        );
        assert_eq!(
            after
                .iter()
                .filter(|r| r.kind == ConversationKind::Regular)
                .count(),
            1,
            "must not mint an extra regular row during bootstrap reject"
        );
        assert_eq!(
            after
                .iter()
                .filter(|r| r.kind == ConversationKind::Delegate)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn effective_guard_allows_unbound_user_root_without_identity() {
        let db = fresh_in_memory_db().await;
        let manager = ConnectionManager::new();
        manager
            .insert_test_connection("user-root", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        // purpose defaults to User; no durable id → brand-new root path.
        ensure_effective_delegate_interactive(&db, &manager, "user-root", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn effective_guard_rejects_identity_disagreement() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        let folder = seed_folder(&db, "/tmp/delegate-access-mismatch").await;
        let other = conversation_service::create(
            &db.conn,
            folder,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        manager
            .insert_test_connection("mismatch", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let state = manager.get_state("mismatch").await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = Some(other.id);
        }
        assert!(matches!(
            ensure_effective_delegate_interactive(
                &db,
                &manager,
                "mismatch",
                Some(child_id),
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })
        ));
    }

    #[tokio::test]
    async fn connect_guard_rejects_session_id_of_locked_child_without_conversation_id() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        conversation_service::update_external_id(&db.conn, child_id, "locked-session".into())
            .await
            .unwrap();
        assert!(matches!(
            ensure_connect_delegate_interactive(
                &db,
                &manager,
                AgentType::Codex,
                Some("locked-session"),
                None,
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::TaskRunning,
            })
        ));
    }

    #[tokio::test]
    async fn connect_guard_rejects_mismatched_conversation_and_session_identity() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        conversation_service::update_external_id(&db.conn, child_id, "child-session".into())
            .await
            .unwrap();
        let folder = seed_folder(&db, "/tmp/delegate-access-connect-mismatch").await;
        let other = conversation_service::create(
            &db.conn,
            folder,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            ensure_connect_delegate_interactive(
                &db,
                &manager,
                AgentType::Codex,
                Some("child-session"),
                Some(other.id),
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })
        ));
    }

    #[tokio::test]
    async fn effective_guard_cross_checks_external_derived_id() {
        let (db, manager, _parent_id, child_id) = fixture().await;
        conversation_service::update_external_id(&db.conn, child_id, "ext-child".into())
            .await
            .unwrap();
        let folder = seed_folder(&db, "/tmp/delegate-access-ext-xcheck").await;
        let other = conversation_service::create(
            &db.conn,
            folder,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        manager
            .insert_test_connection("xcheck", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let state = manager.get_state("xcheck").await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = None;
            s.external_id = Some("ext-child".into());
            s.agent_type = AgentType::Codex;
        }
        // Request id disagrees with durable external_id mapping → state_unknown
        assert!(matches!(
            ensure_effective_delegate_interactive(
                &db,
                &manager,
                "xcheck",
                Some(other.id),
            )
            .await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::StateUnknown,
            })
        ));
    }

    #[tokio::test]
    async fn question_answer_guard_uses_owner_not_caller_connection() {
        // Critical bypass: caller supplies an interactive connection_id while
        // the pending question belongs to a locked delegate connection.
        let (db, manager, parent_id, child_id) = fixture().await;
        manager
            .insert_test_connection("parent-live", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        manager
            .get_state("parent-live")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(parent_id);
        manager
            .insert_test_connection("child-live", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        manager
            .get_state("child-live")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(child_id);

        let reg = manager
            .register_question(
                "child-live",
                vec![crate::acp::question::QuestionSpec {
                    id: "qa".into(),
                    question: "Pick one?".into(),
                    header: "Pick".into(),
                    multi_select: false,
                    options: vec![
                        crate::acp::question::QuestionOption {
                            label: "A".into(),
                            description: String::new(),
                        },
                        crate::acp::question::QuestionOption {
                            label: "B".into(),
                            description: String::new(),
                        },
                    ],
                }],
            )
            .await
            .expect("registered on locked child");

        // Caller connection is interactive — old guard would pass.
        ensure_connection_delegate_interactive(&db, &manager, "parent-live")
            .await
            .expect("parent must be interactive");

        // Authoritative owner guard rejects.
        assert!(matches!(
            ensure_pending_question_delegate_interactive(&db, &manager, &reg.question_id).await,
            Err(crate::acp::error::AcpError::DelegateViewerOnly {
                reason: DelegateAccessReason::TaskRunning,
            })
        ));

        // Rejection must not consume the pending entry.
        assert_eq!(
            manager
                .pending_question_parent_connection_id(&reg.question_id)
                .await
                .as_deref(),
            Some("child-live")
        );
        assert!(manager
            .get_state("child-live")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());
    }
}
