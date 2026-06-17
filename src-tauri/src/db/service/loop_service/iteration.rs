use std::collections::HashMap;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::db::entities::{loop_artifact, loop_issue, loop_iteration};
use crate::db::error::DbError;
use crate::models::loops::LoopIterationRow;

fn to_iteration_row(
    m: &loop_iteration::Model,
    issue_seq: i32,
    target_title: Option<String>,
) -> LoopIterationRow {
    LoopIterationRow {
        id: m.id,
        issue_id: m.issue_id,
        issue_seq,
        stage: m.stage,
        target_artifact_id: m.target_artifact_id,
        target_title,
        conversation_id: m.conversation_id,
        status: m.status,
        launched_by: m.launched_by,
        attempt: m.attempt,
        tokens_used: m.tokens_used,
        created_at: m.created_at,
        started_at: m.started_at,
        ended_at: m.ended_at,
    }
}

pub async fn get_iteration(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
) -> Result<Option<loop_iteration::Model>, DbError> {
    Ok(loop_iteration::Entity::find_by_id(id).one(conn).await?)
}

async fn target_titles(
    conn: &impl sea_orm::ConnectionTrait,
    iterations: &[loop_iteration::Model],
) -> Result<HashMap<i32, String>, DbError> {
    let ids: Vec<i32> = iterations
        .iter()
        .filter_map(|i| i.target_artifact_id)
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(loop_artifact::Entity::find()
        .filter(loop_artifact::Column::Id.is_in(ids))
        .all(conn)
        .await?
        .into_iter()
        .map(|a| (a.id, a.title))
        .collect())
}

pub async fn list_iterations(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    let issue_seq = loop_issue::Entity::find_by_id(issue_id)
        .one(conn)
        .await?
        .map(|i| i.seq_no)
        .unwrap_or(0);
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .order_by_desc(loop_iteration::Column::Id)
        .all(conn)
        .await?;
    let titles = target_titles(conn, &rows).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let title = m
                .target_artifact_id
                .and_then(|tid| titles.get(&tid).cloned());
            to_iteration_row(m, issue_seq, title)
        })
        .collect())
}

/// In-flight (`queued`|`running`) iterations for an issue, ascending by id.
/// Powers the real-time DAG/board ghost nodes + stage rail (spec D1); rides on
/// `LoopDagView.live_iterations` so the graph view is a single authoritative fetch.
pub async fn list_live_for_issue(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    use crate::db::entities::loop_iteration::IterationStatus;
    let issue_seq = loop_issue::Entity::find_by_id(issue_id)
        .one(conn)
        .await?
        .map(|i| i.seq_no)
        .unwrap_or(0);
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(
            loop_iteration::Column::Status
                .is_in([IterationStatus::Queued, IterationStatus::Running]),
        )
        .order_by_asc(loop_iteration::Column::Id)
        .all(conn)
        .await?;
    let titles = target_titles(conn, &rows).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let title = m
                .target_artifact_id
                .and_then(|tid| titles.get(&tid).cloned());
            to_iteration_row(m, issue_seq, title)
        })
        .collect())
}

pub async fn list_iterations_for_space(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    let seqs: HashMap<i32, i32> = loop_issue::Entity::find()
        .filter(loop_issue::Column::SpaceId.eq(space_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|i| (i.id, i.seq_no))
        .collect();
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::SpaceId.eq(space_id))
        .order_by_desc(loop_iteration::Column::Id)
        .all(conn)
        .await?;
    let titles = target_titles(conn, &rows).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let title = m
                .target_artifact_id
                .and_then(|tid| titles.get(&tid).cloned());
            to_iteration_row(m, *seqs.get(&m.issue_id).unwrap_or(&0), title)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::{IterationStatus, Stage};
    use crate::db::service::loop_service::{artifact, issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{
        cas_iteration_status, try_claim_iteration, IterationClaim,
    };
    use crate::models::loops::IssueConfig;

    /// `list_live_for_issue` returns only `queued`|`running` iterations, carrying
    /// stage/target/title — the contract `list_dag.live_iterations` relies on.
    #[tokio::test]
    async fn list_live_returns_only_queued_and_running() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo").await;
        let sp = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(
            &db.conn,
            sp.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            sp.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();

        // A running design iteration → live (carries its target title).
        let running = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Design,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "t1".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cas_iteration_status(
            &db.conn,
            running.id,
            IterationStatus::Queued,
            IterationStatus::Running,
        )
        .await
        .unwrap());

        // A succeeded refine iteration (different stage avoids the active-uniq
        // index) → NOT live.
        let done = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Refine,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "t2".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cas_iteration_status(
            &db.conn,
            done.id,
            IterationStatus::Queued,
            IterationStatus::Running,
        )
        .await
        .unwrap());
        assert!(cas_iteration_status(
            &db.conn,
            done.id,
            IterationStatus::Running,
            IterationStatus::Succeeded,
        )
        .await
        .unwrap());

        let live = list_live_for_issue(&db.conn, iss.row.id).await.unwrap();
        assert_eq!(live.len(), 1, "only queued|running iterations are live");
        assert_eq!(live[0].id, running.id);
        assert_eq!(live[0].stage, Stage::Design);
        assert_eq!(live[0].target_artifact_id, Some(task.id));
        assert_eq!(live[0].target_title.as_deref(), Some("T"));
        assert_eq!(live[0].status, IterationStatus::Running);
    }
}
