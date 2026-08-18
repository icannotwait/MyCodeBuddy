use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

use crate::db::entities::delegation_task_run::{self, Entity as DelegationTaskRun};
use crate::models::AgentType;

use super::types::{
    DelegationOrchestrationBindingPage, DelegationOrchestrationBindingRun,
    OrchestrationBindingQueryError, OrchestrationBindingQueryRequest, OrchestrationBindingV1,
};

pub use super::types::{ORCHESTRATION_BINDING_DEFAULT_LIMIT, ORCHESTRATION_BINDING_MAX_LIMIT};

pub const ORCHESTRATION_BINDING_SNAPSHOT_TTL: Duration = Duration::from_secs(60);
pub const ORCHESTRATION_BINDING_MAX_ROWS: u64 = 4096;

#[derive(Default)]
struct SnapshotState {
    revisions: HashMap<i32, u64>,
    snapshots: HashMap<String, SnapshotEntry>,
}

#[derive(Clone)]
struct SnapshotEntry {
    snapshot_id: String,
    parent_id: i32,
    namespace: String,
    limit: u16,
    revision: u64,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    rows: Vec<DelegationOrchestrationBindingRun>,
    cursors: HashMap<String, usize>,
    cursor_by_start: HashMap<usize, String>,
}

/// Process-local snapshot owner and the mutation fence shared by every run writer.
pub struct OrchestrationBindingSnapshotCache {
    pub(crate) mutation_gate: tokio::sync::RwLock<()>,
    state: tokio::sync::Mutex<SnapshotState>,
}

impl Default for OrchestrationBindingSnapshotCache {
    fn default() -> Self {
        Self {
            mutation_gate: tokio::sync::RwLock::new(()),
            state: tokio::sync::Mutex::new(SnapshotState::default()),
        }
    }
}

impl OrchestrationBindingSnapshotCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn mutation_guard(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.mutation_gate.write().await
    }

    pub(crate) async fn record_parent_mutation(&self, parent_id: i32) {
        let mut state = self.state.lock().await;
        let revision = state.revisions.entry(parent_id).or_default();
        *revision = revision.saturating_add(1);
        state
            .snapshots
            .retain(|_, snapshot| snapshot.parent_id != parent_id);
    }

    pub(crate) async fn page_with_loader<F, Fut>(
        &self,
        parent_id: i32,
        request: OrchestrationBindingQueryRequest,
        now: DateTime<Utc>,
        loader: F,
    ) -> Result<DelegationOrchestrationBindingPage, OrchestrationBindingQueryError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<
            Output = Result<Vec<DelegationOrchestrationBindingRun>, OrchestrationBindingQueryError>,
        >,
    {
        request.validate()?;
        let _read_guard = self.mutation_gate.read().await;

        if let (Some(snapshot_id), Some(cursor)) =
            (request.snapshot_id.as_deref(), request.cursor.as_deref())
        {
            let mut state = self.state.lock().await;
            state
                .snapshots
                .retain(|_, snapshot| snapshot.expires_at > now);
            let current_revision = state.revisions.get(&parent_id).copied().unwrap_or(0);
            let snapshot = state
                .snapshots
                .get(snapshot_id)
                .ok_or(OrchestrationBindingQueryError::SnapshotStale)?;
            if snapshot.parent_id != parent_id || snapshot.revision != current_revision {
                return Err(OrchestrationBindingQueryError::SnapshotStale);
            }
            if snapshot.namespace != request.namespace || snapshot.limit != request.limit {
                return Err(OrchestrationBindingQueryError::Invalid);
            }
            let start = snapshot
                .cursors
                .get(cursor)
                .copied()
                .ok_or(OrchestrationBindingQueryError::Invalid)?;
            return Ok(page_from_snapshot(
                snapshot,
                start,
                Some(cursor.to_string()),
            ));
        }

        let revision = self
            .state
            .lock()
            .await
            .revisions
            .get(&parent_id)
            .copied()
            .unwrap_or(0);
        let mut rows = loader().await?;
        let mut seen = HashSet::with_capacity(rows.len());
        rows.retain(|row| seen.insert(row.task_id.clone()));
        if rows.len() as u64 > ORCHESTRATION_BINDING_MAX_ROWS {
            return Err(OrchestrationBindingQueryError::TooLarge);
        }

        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let expires_at = now
            + chrono::Duration::from_std(ORCHESTRATION_BINDING_SNAPSHOT_TTL)
                .expect("60-second TTL fits chrono");
        let mut cursors = HashMap::new();
        let mut cursor_by_start = HashMap::new();
        for start in (usize::from(request.limit)..rows.len()).step_by(usize::from(request.limit)) {
            let cursor = URL_SAFE_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes());
            cursors.insert(cursor.clone(), start);
            cursor_by_start.insert(start, cursor);
        }
        let snapshot = SnapshotEntry {
            snapshot_id: snapshot_id.clone(),
            parent_id,
            namespace: request.namespace,
            limit: request.limit,
            revision,
            created_at: now,
            expires_at,
            rows,
            cursors,
            cursor_by_start,
        };
        let page = page_from_snapshot(&snapshot, 0, None);
        let mut state = self.state.lock().await;
        state.snapshots.retain(|_, entry| entry.expires_at > now);
        state.snapshots.insert(snapshot_id.clone(), snapshot);
        debug_assert_eq!(page.snapshot_id, snapshot_id);
        Ok(page)
    }
}

fn page_from_snapshot(
    snapshot: &SnapshotEntry,
    start: usize,
    request_cursor: Option<String>,
) -> DelegationOrchestrationBindingPage {
    let end = start
        .saturating_add(usize::from(snapshot.limit))
        .min(snapshot.rows.len());
    let complete = end == snapshot.rows.len();
    DelegationOrchestrationBindingPage {
        schema_version: 1,
        namespace: snapshot.namespace.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        snapshot_revision: snapshot.revision.to_string(),
        snapshot_created_at: snapshot.created_at,
        snapshot_expires_at: snapshot.expires_at,
        total_rows: snapshot.rows.len() as u64,
        page_start: start as u64,
        request_cursor,
        runs: snapshot.rows[start..end].to_vec(),
        next_cursor: (!complete)
            .then(|| snapshot.cursor_by_start.get(&end).cloned())
            .flatten(),
        complete,
    }
}

pub(crate) async fn materialize_binding_rows(
    db: &DatabaseConnection,
    parent_id: i32,
    namespace: &str,
) -> Result<Vec<DelegationOrchestrationBindingRun>, OrchestrationBindingQueryError> {
    let rows = DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent_id))
        .filter(
            Condition::any()
                .add(delegation_task_run::Column::OrchestrationNamespace.eq(namespace))
                .add(delegation_task_run::Column::WorkUnitKey.is_not_null()),
        )
        .order_by_asc(delegation_task_run::Column::CreatedAt)
        .order_by_asc(delegation_task_run::Column::TaskId)
        .limit(ORCHESTRATION_BINDING_MAX_ROWS + 1)
        .all(db)
        .await
        .map_err(|_| OrchestrationBindingQueryError::Failed)?;

    rows.into_iter().map(map_binding_row).collect()
}

fn map_binding_row(
    row: delegation_task_run::Model,
) -> Result<DelegationOrchestrationBindingRun, OrchestrationBindingQueryError> {
    let orchestration_binding = match (
        row.orchestration_schema_version,
        row.orchestration_namespace,
        row.orchestration_generation,
        row.orchestration_route_fingerprint,
    ) {
        (None, None, None, None) => None,
        (Some(schema_version), Some(namespace), Some(generation), Some(route_fingerprint)) => {
            let binding = OrchestrationBindingV1 {
                schema_version: u32::try_from(schema_version)
                    .map_err(|_| OrchestrationBindingQueryError::Failed)?,
                namespace,
                generation: u32::try_from(generation)
                    .map_err(|_| OrchestrationBindingQueryError::Failed)?,
                route_fingerprint,
            };
            binding
                .validate()
                .map_err(|_| OrchestrationBindingQueryError::Failed)?;
            Some(binding)
        }
        _ => return Err(OrchestrationBindingQueryError::Failed),
    };
    let agent_type = AgentType::from_wire(&row.agent_type)
        .ok_or(OrchestrationBindingQueryError::Failed)?
        .as_wire()
        .into_owned();
    let status = serde_json::to_value(row.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or(OrchestrationBindingQueryError::Failed)?;

    Ok(DelegationOrchestrationBindingRun {
        task_id: row.task_id,
        root_task_id: row.root_task_id,
        previous_task_id: row.previous_task_id,
        lineage_root_task_id: row.lineage_root_task_id,
        replaced_task_id: row.replaced_task_id,
        replacement_reason: row.replacement_reason,
        generic_generation: u64::try_from(row.generation)
            .map_err(|_| OrchestrationBindingQueryError::Failed)?,
        work_unit_key: row.work_unit_key,
        child_conversation_id: row.child_conversation_id,
        agent_type,
        profile_id: row.profile_id,
        status,
        orchestration_binding,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
    use serde_json::{json, Value};

    use super::*;
    use crate::acp::delegation::run_store::{PromoteRunningKind, ReservingRunInsert, RunStore};
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::acp::delegation::store::TerminalTaskWrite;
    use crate::acp::delegation::types::OrchestrationBindingQueryRequest;
    use crate::acp::delegation::types::TaskStatus;
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::entities::delegation_task_run::{
        ActiveModel, AdmissionClass, DelegationRunStatus,
    };
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::AgentType;

    fn query(namespace: &str, limit: u16) -> OrchestrationBindingQueryRequest {
        OrchestrationBindingQueryRequest {
            namespace: namespace.into(),
            limit,
            snapshot_id: None,
            cursor: None,
        }
    }

    fn sample_row(task_id: &str) -> DelegationOrchestrationBindingRun {
        DelegationOrchestrationBindingRun {
            task_id: task_id.into(),
            root_task_id: task_id.into(),
            previous_task_id: None,
            lineage_root_task_id: task_id.into(),
            replaced_task_id: None,
            replacement_reason: None,
            generic_generation: 1,
            work_unit_key: Some(format!("task|{task_id}")),
            child_conversation_id: 1,
            agent_type: "codex".into(),
            profile_id: None,
            status: "running".into(),
            orchestration_binding: None,
        }
    }

    #[test]
    fn orchestration_binding_query_input_is_strict_and_bounded() {
        let defaulted: OrchestrationBindingQueryRequest = serde_json::from_value(json!({
            "namespace": "brainstorm-to-delivery"
        }))
        .unwrap();
        assert_eq!(defaulted.limit, ORCHESTRATION_BINDING_DEFAULT_LIMIT);
        assert!(defaulted.validate().is_ok());
        assert!(
            serde_json::from_value::<OrchestrationBindingQueryRequest>(json!({
                "namespace": "brainstorm-to-delivery",
                "parent_conversation_id": 42
            }))
            .is_err()
        );

        for namespace in ["", "Upper", "bad_name", &"a".repeat(65)] {
            assert_eq!(
                query(namespace, 100).validate(),
                Err(OrchestrationBindingQueryError::Invalid)
            );
        }
        for limit in [1, 200] {
            assert!(query("brainstorm-to-delivery", limit).validate().is_ok());
        }
        for limit in [0, 201] {
            assert_eq!(
                query("brainstorm-to-delivery", limit).validate(),
                Err(OrchestrationBindingQueryError::Invalid)
            );
        }

        let snapshot_id = uuid::Uuid::new_v4().to_string();
        for (snapshot, cursor) in [
            (Some(snapshot_id.clone()), None),
            (None, Some("abc".into())),
            (Some("not-a-uuid".into()), Some("abc".into())),
            (Some(snapshot_id.clone()), Some(String::new())),
            (Some(snapshot_id.clone()), Some("a".repeat(129))),
            (Some(snapshot_id), Some("not+base64url".into())),
        ] {
            let request = OrchestrationBindingQueryRequest {
                snapshot_id: snapshot,
                cursor,
                ..query("brainstorm-to-delivery", 100)
            };
            assert_eq!(
                request.validate(),
                Err(OrchestrationBindingQueryError::Invalid)
            );
        }
    }

    #[tokio::test]
    async fn orchestration_binding_query_pages_are_stable_replayable_and_parent_scoped() {
        let cache = OrchestrationBindingSnapshotCache::new();
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 8, 0, 0).unwrap();
        let rows = vec![sample_row("a"), sample_row("b"), sample_row("c")];
        let first = cache
            .page_with_loader(10, query("brainstorm-to-delivery", 2), now, || async {
                Ok(rows)
            })
            .await
            .unwrap();
        assert_eq!(first.schema_version, 1);
        assert_eq!(first.snapshot_revision, "0");
        assert_eq!(first.snapshot_created_at, now);
        assert_eq!(
            first.snapshot_expires_at,
            now + chrono::Duration::seconds(60)
        );
        assert_eq!(first.total_rows, 3);
        assert_eq!(first.page_start, 0);
        assert_eq!(first.request_cursor, None);
        assert_eq!(first.runs.len(), 2);
        assert!(!first.complete);
        let cursor = first.next_cursor.clone().expect("non-final cursor");
        assert!((1..=128).contains(&cursor.len()));
        assert!(cursor
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));

        let later_request = OrchestrationBindingQueryRequest {
            namespace: first.namespace.clone(),
            limit: 2,
            snapshot_id: Some(first.snapshot_id.clone()),
            cursor: Some(cursor.clone()),
        };
        let later = cache
            .page_with_loader(10, later_request.clone(), now, || async {
                panic!("later pages must not rematerialize")
            })
            .await
            .unwrap();
        assert_eq!(later.page_start, 2);
        assert_eq!(later.request_cursor.as_deref(), Some(cursor.as_str()));
        assert_eq!(later.next_cursor, None);
        assert!(later.complete);
        assert_eq!(later.runs.len(), 1);
        assert_eq!(
            cache
                .page_with_loader(10, later_request.clone(), now, || async {
                    panic!("replay must use the cache")
                })
                .await
                .unwrap(),
            later
        );

        for changed in [
            OrchestrationBindingQueryRequest {
                namespace: "another-namespace".into(),
                ..later_request.clone()
            },
            OrchestrationBindingQueryRequest {
                limit: 1,
                ..later_request.clone()
            },
        ] {
            assert_eq!(
                cache
                    .page_with_loader(10, changed, now, || async { unreachable!() })
                    .await,
                Err(OrchestrationBindingQueryError::Invalid)
            );
        }
        assert_eq!(
            cache
                .page_with_loader(11, later_request, now, || async { unreachable!() })
                .await,
            Err(OrchestrationBindingQueryError::SnapshotStale)
        );
    }

    #[tokio::test]
    async fn orchestration_binding_query_revision_expiry_and_restart_are_stale_without_pages() {
        let cache = OrchestrationBindingSnapshotCache::new();
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 8, 0, 0).unwrap();
        let first = cache
            .page_with_loader(20, query("brainstorm-to-delivery", 1), now, || async {
                Ok(vec![sample_row("a"), sample_row("b")])
            })
            .await
            .unwrap();
        let continuation = OrchestrationBindingQueryRequest {
            namespace: first.namespace,
            limit: 1,
            snapshot_id: Some(first.snapshot_id),
            cursor: first.next_cursor,
        };

        let restarted = OrchestrationBindingSnapshotCache::new();
        assert_eq!(
            restarted
                .page_with_loader(20, continuation.clone(), now, || async { unreachable!() })
                .await,
            Err(OrchestrationBindingQueryError::SnapshotStale)
        );
        assert_eq!(
            cache
                .page_with_loader(
                    20,
                    continuation.clone(),
                    now + chrono::Duration::seconds(60),
                    || async { unreachable!() },
                )
                .await,
            Err(OrchestrationBindingQueryError::SnapshotStale)
        );

        let fresh = cache
            .page_with_loader(20, query("brainstorm-to-delivery", 1), now, || async {
                Ok(vec![sample_row("c"), sample_row("d")])
            })
            .await
            .unwrap();
        let stale = OrchestrationBindingQueryRequest {
            namespace: fresh.namespace,
            limit: 1,
            snapshot_id: Some(fresh.snapshot_id),
            cursor: fresh.next_cursor,
        };
        let guard = cache.mutation_guard().await;
        cache.record_parent_mutation(20).await;
        drop(guard);
        assert_eq!(
            cache
                .page_with_loader(20, stale, now, || async { unreachable!() })
                .await,
            Err(OrchestrationBindingQueryError::SnapshotStale)
        );
    }

    async fn parent_child() -> (Arc<crate::db::AppDatabase>, i32, i32) {
        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-binding-query").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "query-parent-tool".into(),
                delegation_call_id: "query-call".into(),
            }),
        )
        .await
        .unwrap();
        (db, parent.id, child.id)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_row(
        db: &crate::db::AppDatabase,
        parent_id: i32,
        child_id: i32,
        task_id: &str,
        generation: i64,
        created_at: DateTime<Utc>,
        work_unit_key: Option<&str>,
        binding: Option<OrchestrationBindingV1>,
        agent_type: &str,
        profile_id: Option<&str>,
    ) {
        let (schema_version, namespace, binding_generation, route_fingerprint) = binding
            .map(|binding| {
                (
                    Some(i64::from(binding.schema_version)),
                    Some(binding.namespace),
                    Some(i64::from(binding.generation)),
                    Some(binding.route_fingerprint),
                )
            })
            .unwrap_or((None, None, None, None));
        ActiveModel {
            task_id: Set(task_id.into()),
            root_task_id: Set(format!("root-{task_id}")),
            previous_task_id: Set(Some(format!("previous-{task_id}"))),
            generation: Set(generation),
            parent_conversation_id: Set(parent_id),
            child_conversation_id: Set(child_id),
            agent_type: Set(agent_type.into()),
            profile_id: Set(profile_id.map(str::to_string)),
            orchestration_schema_version: Set(schema_version),
            orchestration_namespace: Set(namespace),
            orchestration_generation: Set(binding_generation),
            orchestration_route_fingerprint: Set(route_fingerprint),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set(format!("lineage-{task_id}")),
            work_unit_key: Set(work_unit_key.map(str::to_string)),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            replaced_task_id: Set(Some(format!("replaced-{task_id}"))),
            replacement_reason: Set(Some("unresumable".into())),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }

    fn binding(namespace: &str, generation: u32) -> OrchestrationBindingV1 {
        OrchestrationBindingV1 {
            schema_version: 1,
            namespace: namespace.into(),
            generation,
            route_fingerprint: format!("sha256:{}", "a".repeat(64)),
        }
    }

    fn reserving_insert(
        task_id: &str,
        parent_id: i32,
        child_id: i32,
        generation: i64,
    ) -> ReservingRunInsert {
        ReservingRunInsert {
            task_id: task_id.into(),
            root_task_id: task_id.into(),
            previous_task_id: None,
            generation,
            parent_conversation_id: parent_id,
            parent_tool_use_id: Some(format!("tool-{task_id}")),
            child_conversation_id: child_id,
            agent_type: "codex".into(),
            profile_id: Some("actual-profile".into()),
            orchestration_binding: Some(binding("brainstorm-to-delivery", 1)),
            workspace_path: Some("/tmp/codeg-binding-query".into()),
            route_fingerprint: Some("launch-route".into()),
            launch_snapshot_version: None,
            mode_id: None,
            config_values_json: None,
            task_preview: Some("private preview".into()),
            request_fingerprint: Some(format!("fingerprint-{task_id}")),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: task_id.into(),
            work_unit_key: Some(format!("task|{task_id}")),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        }
    }

    async fn continuation_request(
        store: &RunStore,
        parent_id: i32,
    ) -> OrchestrationBindingQueryRequest {
        let page = store
            .get_orchestration_binding_page(parent_id, query("brainstorm-to-delivery", 1))
            .await
            .unwrap();
        OrchestrationBindingQueryRequest {
            namespace: page.namespace,
            limit: 1,
            snapshot_id: Some(page.snapshot_id),
            cursor: Some(page.next_cursor.expect("fixture has multiple rows")),
        }
    }

    async fn assert_stale(
        store: &RunStore,
        parent_id: i32,
        request: OrchestrationBindingQueryRequest,
    ) {
        assert_eq!(
            store
                .get_orchestration_binding_page(parent_id, request)
                .await,
            Err(OrchestrationBindingQueryError::SnapshotStale)
        );
    }

    #[tokio::test]
    async fn orchestration_binding_query_run_mutations_invalidate_after_commit() {
        let (db, parent_id, child_id) = parent_child().await;
        let base = Utc.with_ymd_and_hms(2026, 8, 17, 8, 0, 0).unwrap();
        for (index, task_id) in ["seed-a", "seed-b"].into_iter().enumerate() {
            insert_row(
                &db,
                parent_id,
                child_id,
                task_id,
                index as i64 + 1,
                base + chrono::Duration::seconds(index as i64),
                Some(task_id),
                Some(binding("brainstorm-to-delivery", 1)),
                "codex",
                None,
            )
            .await;
        }
        let store = RunStore::new(db);

        let before_insert = continuation_request(&store, parent_id).await;
        store
            .insert_reserving(reserving_insert("inserted", parent_id, child_id, 10))
            .await
            .unwrap();
        assert_stale(&store, parent_id, before_insert).await;

        let before_promote = continuation_request(&store, parent_id).await;
        store
            .bind_child_connection_while_reserving("inserted", "connection-inserted")
            .await
            .unwrap();
        store
            .promote_running("inserted", "connection-inserted", Utc::now())
            .await
            .unwrap();
        assert_stale(&store, parent_id, before_promote).await;

        let before_terminal = continuation_request(&store, parent_id).await;
        store
            .settle_terminal(
                "inserted",
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        assert_stale(&store, parent_id, before_terminal).await;

        store
            .insert_reserving(reserving_insert("pre-admission", parent_id, child_id, 11))
            .await
            .unwrap();
        let before_pre_admission = continuation_request(&store, parent_id).await;
        store
            .settle_pre_admission_failure_if_owned(
                "pre-admission",
                "connection-pre-admission",
                TerminalTaskWrite::failed(
                    "spawn_failed",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .unwrap();
        assert_stale(&store, parent_id, before_pre_admission).await;

        store
            .insert_reserving(reserving_insert("cancel-cleanup", parent_id, child_id, 12))
            .await
            .unwrap();
        let before_cancel = continuation_request(&store, parent_id).await;
        store
            .settle_terminal(
                "cancel-cleanup",
                TerminalTaskWrite::legacy_without_audit(
                    TaskStatus::Canceled,
                    Some("user_cancelled".into()),
                ),
            )
            .await
            .unwrap();
        assert_stale(&store, parent_id, before_cancel).await;

        store
            .insert_reserving(reserving_insert("deleted", parent_id, child_id, 13))
            .await
            .unwrap();
        let before_delete = continuation_request(&store, parent_id).await;
        assert!(store.abandon_reserving_claim("deleted").await.unwrap());
        assert_stale(&store, parent_id, before_delete).await;
    }

    #[tokio::test]
    async fn orchestration_binding_query_promote_invalidates_when_unlocked_pre_read_fails() {
        let (db, parent_id, child_id) = parent_child().await;
        insert_row(
            &db,
            parent_id,
            child_id,
            "seed",
            1,
            Utc.with_ymd_and_hms(2026, 8, 17, 8, 0, 0).unwrap(),
            Some("task|seed"),
            Some(binding("brainstorm-to-delivery", 1)),
            "codex",
            None,
        )
        .await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(reserving_insert("promote", parent_id, child_id, 2))
            .await
            .unwrap();
        let before_promote = continuation_request(&store, parent_id).await;
        store
            .bind_child_connection_while_reserving("promote", "connection-promote")
            .await
            .unwrap();
        store.fail_next_load_by_task_id();

        let outcome = store
            .promote_running_detailed("promote", "connection-promote", Utc::now())
            .await
            .unwrap();
        assert!(matches!(outcome.kind, PromoteRunningKind::Promoted { .. }));
        let durable = DelegationTaskRun::find_by_id("promote")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(durable.status, DelegationRunStatus::Running);
        assert_stale(&store, parent_id, before_promote).await;
    }

    #[tokio::test]
    async fn orchestration_binding_query_materializes_exact_conflict_set_order_and_redaction() {
        let (db, parent_id, child_id) = parent_child().await;
        let base = Utc.with_ymd_and_hms(2026, 8, 17, 8, 0, 0).unwrap();
        insert_row(
            &db,
            parent_id,
            child_id,
            "a-requested-unkeyed",
            1,
            base,
            None,
            Some(binding("brainstorm-to-delivery", 2)),
            "grok",
            Some("profile-real"),
        )
        .await;
        insert_row(
            &db,
            parent_id,
            child_id,
            "b-requested-keyed",
            2,
            base,
            Some("task|2|reviewer|codex|none"),
            Some(binding("brainstorm-to-delivery", 2)),
            "codex",
            None,
        )
        .await;
        insert_row(
            &db,
            parent_id,
            child_id,
            "c-unbound-keyed",
            3,
            base + chrono::Duration::seconds(1),
            Some("task|3|implementer|codex|none"),
            None,
            "codex",
            None,
        )
        .await;
        insert_row(
            &db,
            parent_id,
            child_id,
            "d-foreign-keyed",
            4,
            base + chrono::Duration::seconds(2),
            Some("task|4|reviewer|grok|none"),
            Some(binding("foreign-namespace", 1)),
            "grok",
            None,
        )
        .await;
        insert_row(
            &db,
            parent_id,
            child_id,
            "e-foreign-unkeyed",
            5,
            base + chrono::Duration::seconds(3),
            None,
            Some(binding("foreign-namespace", 1)),
            "codex",
            None,
        )
        .await;

        let cache = OrchestrationBindingSnapshotCache::new();
        let namespace = "brainstorm-to-delivery".to_string();
        let first = cache
            .page_with_loader(parent_id, query(&namespace, 2), base, || async {
                materialize_binding_rows(&db.conn, parent_id, &namespace).await
            })
            .await
            .unwrap();
        assert_eq!(
            first.total_rows, 4,
            "union must deduplicate keyed same-namespace rows"
        );
        assert_eq!(
            first
                .runs
                .iter()
                .map(|row| row.task_id.as_str())
                .collect::<Vec<_>>(),
            ["a-requested-unkeyed", "b-requested-keyed"]
        );
        assert_eq!(first.runs[0].agent_type, "grok");
        assert_eq!(first.runs[0].profile_id.as_deref(), Some("profile-real"));
        assert_eq!(
            first.runs[0].previous_task_id.as_deref(),
            Some("previous-a-requested-unkeyed")
        );
        assert_eq!(first.runs[0].status, "completed");

        let second_request = OrchestrationBindingQueryRequest {
            namespace: first.namespace.clone(),
            limit: 2,
            snapshot_id: Some(first.snapshot_id.clone()),
            cursor: first.next_cursor.clone(),
        };
        let second = cache
            .page_with_loader(parent_id, second_request, base, || async { unreachable!() })
            .await
            .unwrap();
        assert_eq!(
            second
                .runs
                .iter()
                .map(|row| row.task_id.as_str())
                .collect::<Vec<_>>(),
            ["c-unbound-keyed", "d-foreign-keyed"]
        );
        assert!(second.runs[0].orchestration_binding.is_none());
        assert_eq!(
            second.runs[1]
                .orchestration_binding
                .as_ref()
                .unwrap()
                .namespace,
            "foreign-namespace"
        );

        let serialized = serde_json::to_value([first, second]).unwrap();
        let forbidden = [
            "prompt",
            "task_preview",
            "output",
            "result",
            "termination_audit_json",
            "card_summary_json",
            "completion_evidence_json",
            "config_values_json",
            "profile_config",
        ];
        fn scan(value: &Value, forbidden: &[&str]) {
            match value {
                Value::Object(object) => {
                    for (key, value) in object {
                        assert!(!forbidden.contains(&key.as_str()), "leaked key {key}");
                        scan(value, forbidden);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        scan(value, forbidden);
                    }
                }
                _ => {}
            }
        }
        scan(&serialized, &forbidden);
    }

    async fn seed_cap_rows(count: u64) -> (Arc<crate::db::AppDatabase>, i32) {
        let (db, parent_id, child_id) = parent_child().await;
        let sql = format!(
            "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM seq WHERE x < {count}) \
             INSERT INTO delegation_task_runs (task_id, root_task_id, generation, \
             parent_conversation_id, child_conversation_id, agent_type, admission_class, \
             lineage_root_task_id, work_unit_key, history_only, status, created_at, updated_at) \
             SELECT printf('cap-%05d', x), printf('cap-%05d', x), x, {parent_id}, {child_id}, \
             'codex', 'normal_revision', printf('cap-%05d', x), printf('unit-%05d', x), 0, \
             'completed', printf('2026-08-17T08:%02d:%02dZ', (x / 60) % 60, x % 60), \
             printf('2026-08-17T08:%02d:%02dZ', (x / 60) % 60, x % 60) FROM seq"
        );
        db.conn.execute_unprepared(&sql).await.unwrap();
        (db, parent_id)
    }

    #[tokio::test]
    async fn orchestration_binding_query_row_cap_is_reject_not_truncate() {
        let (within_db, within_parent) = seed_cap_rows(4096).await;
        let within =
            materialize_binding_rows(&within_db.conn, within_parent, "brainstorm-to-delivery")
                .await
                .unwrap();
        assert_eq!(within.len(), 4096);
        let cache = OrchestrationBindingSnapshotCache::new();
        let page = cache
            .page_with_loader(
                within_parent,
                query("brainstorm-to-delivery", 200),
                Utc::now(),
                || async { Ok(within) },
            )
            .await
            .unwrap();
        assert_eq!(page.total_rows, 4096);

        let (over_db, over_parent) = seed_cap_rows(4097).await;
        let rows = materialize_binding_rows(&over_db.conn, over_parent, "brainstorm-to-delivery")
            .await
            .unwrap();
        assert_eq!(rows.len(), 4097);
        assert_eq!(
            cache
                .page_with_loader(
                    over_parent,
                    query("brainstorm-to-delivery", 200),
                    Utc::now(),
                    || async { Ok(rows) },
                )
                .await,
            Err(OrchestrationBindingQueryError::TooLarge)
        );
    }

    #[tokio::test]
    async fn orchestration_binding_query_db_and_partial_materialization_fail_without_page() {
        let (db, parent_id, child_id) = parent_child().await;
        db.conn
            .execute_unprepared("DROP TRIGGER trg_dtr_orchestration_binding_shape")
            .await
            .unwrap();
        ActiveModel {
            task_id: Set("partial".into()),
            root_task_id: Set("partial".into()),
            generation: Set(1),
            parent_conversation_id: Set(parent_id),
            child_conversation_id: Set(child_id),
            agent_type: Set("codex".into()),
            orchestration_schema_version: Set(Some(1)),
            orchestration_namespace: Set(None),
            orchestration_generation: Set(None),
            orchestration_route_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set("partial".into()),
            work_unit_key: Set(Some("task|partial".into())),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        assert_eq!(
            materialize_binding_rows(&db.conn, parent_id, "brainstorm-to-delivery").await,
            Err(OrchestrationBindingQueryError::Failed)
        );

        db.conn
            .execute_unprepared("DROP TABLE delegation_task_runs")
            .await
            .unwrap();
        assert_eq!(
            materialize_binding_rows(&db.conn, parent_id, "brainstorm-to-delivery").await,
            Err(OrchestrationBindingQueryError::Failed)
        );
    }
}
