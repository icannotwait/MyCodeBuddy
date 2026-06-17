use std::collections::HashSet;

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_criterion::{self, CriterionKind};
use crate::db::entities::loop_coverage;
use crate::db::error::DbError;
use crate::models::loops::LoopCoverageRow;

pub fn to_coverage_row(m: loop_coverage::Model) -> LoopCoverageRow {
    LoopCoverageRow {
        id: m.id,
        task_artifact_id: m.task_artifact_id,
        criterion_id: m.criterion_id,
    }
}

/// Idempotent: a repeated `(task, criterion)` pair returns the existing row
/// instead of inserting a duplicate (also guarded by `uniq_loop_coverage`).
pub async fn create_coverage(
    conn: &impl sea_orm::ConnectionTrait,
    space_id: i32,
    task_artifact_id: i32,
    criterion_id: i32,
) -> Result<loop_coverage::Model, DbError> {
    if let Some(existing) = loop_coverage::Entity::find()
        .filter(loop_coverage::Column::TaskArtifactId.eq(task_artifact_id))
        .filter(loop_coverage::Column::CriterionId.eq(criterion_id))
        .one(conn)
        .await?
    {
        return Ok(existing);
    }
    Ok(loop_coverage::ActiveModel {
        space_id: Set(space_id),
        task_artifact_id: Set(task_artifact_id),
        criterion_id: Set(criterion_id),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

/// All coverage edges whose task artifact belongs to `issue_id`. Joined through
/// the artifact's `issue_id` (coverage carries only `space_id`, not `issue_id`).
pub async fn list_for_issue(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
) -> Result<Vec<LoopCoverageRow>, DbError> {
    let task_ids: Vec<i32> = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|m| m.id)
        .collect();
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(loop_coverage::Entity::find()
        .filter(loop_coverage::Column::TaskArtifactId.is_in(task_ids))
        .all(conn)
        .await?
        .into_iter()
        .map(to_coverage_row)
        .collect())
}

/// Ordered `(requirement_id, [acceptance criterion ids])` for an issue's live
/// (non-superseded/cancelled) done requirements — the single source of the
/// stable `R{i}.AC{j}` coverage ordinals. Requirements ordered by `(sort, id)`,
/// criteria by `(sort, id)`. ingest's `covers` map, the driver's coverage gate,
/// and the planner briefing all build their ordinals from this one function, so
/// `R1.AC1` means the same criterion everywhere.
pub async fn acceptance_ordinals_for_issue(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<(i32, Vec<i32>)>, DbError> {
    let reqs = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue_id))
        .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Requirement))
        .filter(loop_artifact::Column::Status.eq(ArtifactStatus::Done))
        .order_by_asc(loop_artifact::Column::Sort)
        .order_by_asc(loop_artifact::Column::Id)
        .all(conn)
        .await?;
    let mut out = Vec::with_capacity(reqs.len());
    for r in reqs {
        let crits = loop_criterion::Entity::find()
            .filter(loop_criterion::Column::ArtifactId.eq(r.id))
            .filter(loop_criterion::Column::Kind.eq(CriterionKind::Acceptance))
            .order_by_asc(loop_criterion::Column::Sort)
            .order_by_asc(loop_criterion::Column::Id)
            .all(conn)
            .await?;
        out.push((r.id, crits.into_iter().map(|c| c.id).collect()));
    }
    Ok(out)
}

/// The `R{i}.AC{j}` ordinals (1-based) whose criterion no *live* task covers.
/// Empty ⇒ coverage complete (vacuously so when there are no acceptance
/// criteria, e.g. the direct route). Pure — the driver's bounded replan
/// loop-back fires whenever this is non-empty.
pub fn uncovered_ordinals(
    ordinals: &[(i32, Vec<i32>)],
    coverage: &[LoopCoverageRow],
    live_tasks: &HashSet<i32>,
) -> Vec<String> {
    let covered: HashSet<i32> = coverage
        .iter()
        .filter(|c| live_tasks.contains(&c.task_artifact_id))
        .map(|c| c.criterion_id)
        .collect();
    let mut out = Vec::new();
    for (ri, (_req, crits)) in ordinals.iter().enumerate() {
        for (ci, cid) in crits.iter().enumerate() {
            if !covered.contains(cid) {
                out.push(format!("R{}.AC{}", ri + 1, ci + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cov(task: i32, crit: i32) -> LoopCoverageRow {
        LoopCoverageRow {
            id: 0,
            task_artifact_id: task,
            criterion_id: crit,
        }
    }

    #[test]
    fn uncovered_ordinals_reports_only_gaps_from_live_tasks() {
        // R1 has AC1(=10), AC2(=11); R2 has AC1(=20).
        let ordinals = vec![(1, vec![10, 11]), (2, vec![20])];
        let live: HashSet<i32> = [100, 101].into_iter().collect();

        // Full coverage by live tasks → no gaps.
        let full = vec![cov(100, 10), cov(100, 11), cov(101, 20)];
        assert!(uncovered_ordinals(&ordinals, &full, &live).is_empty());

        // R1.AC2 uncovered.
        let partial = vec![cov(100, 10), cov(101, 20)];
        assert_eq!(uncovered_ordinals(&ordinals, &partial, &live), vec!["R1.AC2"]);

        // Coverage by a non-live (superseded) task doesn't count.
        let stale = vec![cov(100, 10), cov(100, 11), cov(999, 20)];
        assert_eq!(uncovered_ordinals(&ordinals, &stale, &live), vec!["R2.AC1"]);

        // No requirements → vacuously complete.
        assert!(uncovered_ordinals(&[], &full, &live).is_empty());
    }
}
