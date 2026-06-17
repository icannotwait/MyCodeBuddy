use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::db::entities::loop_artifact;
use crate::db::entities::loop_criterion_check::{self, CheckVerdict};
use crate::db::error::DbError;
use crate::models::loops::LoopCriterionCheckRow;

pub fn to_check_row(m: loop_criterion_check::Model) -> LoopCriterionCheckRow {
    LoopCriterionCheckRow {
        id: m.id,
        criterion_id: m.criterion_id,
        iteration_id: m.iteration_id,
        scope_artifact_id: m.scope_artifact_id,
        verdict: m.verdict,
        evidence: m.evidence,
    }
}

/// Idempotent on `(criterion, iteration, scope)` (also guarded by
/// `uniq_loop_criterion_check`): a crash replay of a review submission returns
/// the existing check instead of inserting a duplicate.
pub async fn create_check(
    conn: &impl sea_orm::ConnectionTrait,
    space_id: i32,
    criterion_id: i32,
    iteration_id: i32,
    scope_artifact_id: i32,
    verdict: CheckVerdict,
    evidence: &str,
) -> Result<loop_criterion_check::Model, DbError> {
    if let Some(existing) = loop_criterion_check::Entity::find()
        .filter(loop_criterion_check::Column::CriterionId.eq(criterion_id))
        .filter(loop_criterion_check::Column::IterationId.eq(iteration_id))
        .filter(loop_criterion_check::Column::ScopeArtifactId.eq(scope_artifact_id))
        .one(conn)
        .await?
    {
        return Ok(existing);
    }
    Ok(loop_criterion_check::ActiveModel {
        space_id: Set(space_id),
        criterion_id: Set(criterion_id),
        iteration_id: Set(iteration_id),
        scope_artifact_id: Set(scope_artifact_id),
        verdict: Set(verdict),
        evidence: Set(evidence.to_string()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

/// All checks whose scope artifact belongs to `issue_id` — the per-issue trace
/// matrix (checks carry only `space_id`, so the issue is resolved via the scope
/// artifact).
pub async fn list_for_issue(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
) -> Result<Vec<LoopCriterionCheckRow>, DbError> {
    let art_ids: Vec<i32> = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|m| m.id)
        .collect();
    if art_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(loop_criterion_check::Entity::find()
        .filter(loop_criterion_check::Column::ScopeArtifactId.is_in(art_ids))
        .all(conn)
        .await?
        .into_iter()
        .map(to_check_row)
        .collect())
}

/// Checks for one scope artifact produced by the given review iterations — the
/// inputs the gate aggregates for one deciding attempt.
pub async fn for_scope_iterations(
    conn: &impl sea_orm::ConnectionTrait,
    scope_artifact_id: i32,
    iteration_ids: &[i32],
) -> Result<Vec<LoopCriterionCheckRow>, DbError> {
    if iteration_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(loop_criterion_check::Entity::find()
        .filter(loop_criterion_check::Column::ScopeArtifactId.eq(scope_artifact_id))
        .filter(loop_criterion_check::Column::IterationId.is_in(iteration_ids.to_vec()))
        .all(conn)
        .await?
        .into_iter()
        .map(to_check_row)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_criterion::CriterionKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::Stage;
    use crate::db::service::loop_service::{artifact, issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{try_claim_iteration, IterationClaim};
    use crate::models::loops::IssueConfig;

    #[tokio::test]
    async fn create_check_is_idempotent_on_the_unique_key() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo").await;
        let space = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            space.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::InProgress,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let crit = artifact::add_criterion(&db.conn, task.id, CriterionKind::Acceptance, "ac")
            .await
            .unwrap();
        let it = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: space.id,
                issue_id: iss.row.id,
                stage: Stage::Review,
                target_artifact_id: Some(task.id),
                slot_no: Some(0),
                capability_token: "tok".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();

        let a = create_check(&db.conn, space.id, crit.id, it.id, task.id, CheckVerdict::Pass, "ok")
            .await
            .unwrap();
        let b = create_check(
            &db.conn,
            space.id,
            crit.id,
            it.id,
            task.id,
            CheckVerdict::Fail,
            "changed",
        )
        .await
        .unwrap();
        assert_eq!(a.id, b.id, "same (criterion,iteration,scope) returns existing");
        assert_eq!(b.verdict, CheckVerdict::Pass, "first write wins; no overwrite");
    }
}
