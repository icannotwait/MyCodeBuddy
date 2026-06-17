use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};

use crate::db::entities::loop_gate_decision::{self, GateOutcome};
use crate::db::entities::loop_criterion_check::CheckVerdict;
use crate::db::error::DbError;
use crate::models::loops::{LoopCriterionCheckRow, LoopGateDecisionRow};

pub fn to_decision_row(m: loop_gate_decision::Model) -> LoopGateDecisionRow {
    LoopGateDecisionRow {
        id: m.id,
        target_artifact_id: m.target_artifact_id,
        stage: m.stage,
        attempt: m.attempt,
        outcome: m.outcome,
        input_check_ids: serde_json::from_str(&m.input_check_ids).unwrap_or_default(),
        created_at: m.created_at.to_rfc3339(),
    }
}

fn verdict_str(v: CheckVerdict) -> &'static str {
    match v {
        CheckVerdict::Pass => "pass",
        CheckVerdict::Fail => "fail",
    }
}

/// Canonical, order-independent fingerprint of a gate's inputs: every aggregated
/// check as `(criterion, scope, iteration, verdict)` (iteration = stable
/// per-attempt reviewer identity, never submission order), the injected criterion
/// id set, and the policy. Two ticks that aggregate the same inputs produce the
/// same digest, so a replay is idempotent and a divergent recompute is detectable.
pub fn canonical_digest(
    checks: &[LoopCriterionCheckRow],
    injected_ids: &[i32],
    policy_json: &str,
) -> String {
    let mut tuples: Vec<String> = checks
        .iter()
        .map(|c| {
            format!(
                "{}:{}:{}:{}",
                c.criterion_id,
                c.scope_artifact_id,
                c.iteration_id,
                verdict_str(c.verdict)
            )
        })
        .collect();
    tuples.sort();
    let mut inj: Vec<i32> = injected_ids.to_vec();
    inj.sort_unstable();
    inj.dedup();
    format!("c=[{}]|i={inj:?}|p={policy_json}", tuples.join(","))
}

/// Outcome of recording a decision under the `(target, stage, attempt)` unique key.
pub enum RecordedDecision {
    /// Inserted now, or an existing row whose digest matches (idempotent replay).
    Settled(loop_gate_decision::Model),
    /// A row already exists for this key with a DIFFERENT digest — a racing
    /// recompute aggregated different inputs. The caller must re-tick against
    /// fresh state; the recorded decision is never silently overwritten.
    Conflict(loop_gate_decision::Model),
}

/// Insert-or-compare the immutable gate decision (§3.4). The `(target, stage,
/// attempt)` unique index makes this the durable pivot: a replay with the same
/// inputs returns `Settled(existing)`; a recompute with different inputs returns
/// `Conflict(existing)`.
#[allow(clippy::too_many_arguments)]
pub async fn record_decision(
    conn: &impl sea_orm::ConnectionTrait,
    space_id: i32,
    issue_id: i32,
    target_artifact_id: i32,
    stage: &str,
    attempt: i32,
    checks: &[LoopCriterionCheckRow],
    injected_ids: &[i32],
    policy_json: &str,
    outcome: GateOutcome,
) -> Result<RecordedDecision, DbError> {
    let digest = canonical_digest(checks, injected_ids, policy_json);
    let mut ids: Vec<i32> = checks.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let ids_json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());

    let settle = |existing: loop_gate_decision::Model| {
        if existing.input_digest == digest {
            RecordedDecision::Settled(existing)
        } else {
            RecordedDecision::Conflict(existing)
        }
    };

    if let Some(existing) = find_decision(conn, target_artifact_id, stage, attempt).await? {
        return Ok(settle(existing));
    }
    let am = loop_gate_decision::ActiveModel {
        space_id: Set(space_id),
        issue_id: Set(issue_id),
        target_artifact_id: Set(target_artifact_id),
        stage: Set(stage.to_string()),
        attempt: Set(attempt),
        policy_json: Set(policy_json.to_string()),
        input_check_ids: Set(ids_json),
        input_digest: Set(digest.clone()),
        outcome: Set(outcome),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    match am.insert(conn).await {
        Ok(m) => Ok(RecordedDecision::Settled(m)),
        Err(e) => {
            // A racing insert may have won the unique key; re-read and compare
            // instead of surfacing the violation as a hard error.
            if let Some(existing) = find_decision(conn, target_artifact_id, stage, attempt).await? {
                Ok(settle(existing))
            } else {
                Err(e.into())
            }
        }
    }
}

async fn find_decision(
    conn: &impl sea_orm::ConnectionTrait,
    target_artifact_id: i32,
    stage: &str,
    attempt: i32,
) -> Result<Option<loop_gate_decision::Model>, DbError> {
    Ok(loop_gate_decision::Entity::find()
        .filter(loop_gate_decision::Column::TargetArtifactId.eq(target_artifact_id))
        .filter(loop_gate_decision::Column::Stage.eq(stage))
        .filter(loop_gate_decision::Column::Attempt.eq(attempt))
        .one(conn)
        .await?)
}

/// The recorded outcome for a gate at `(target, stage, attempt)`, if any.
pub async fn outcome_for(
    conn: &impl sea_orm::ConnectionTrait,
    target_artifact_id: i32,
    stage: &str,
    attempt: i32,
) -> Result<Option<GateOutcome>, DbError> {
    Ok(find_decision(conn, target_artifact_id, stage, attempt)
        .await?
        .map(|m| m.outcome))
}

/// Number of `fail` decisions recorded for an issue at a stage — the bound for the
/// integration loop-back (counts failed integration attempts durably).
pub async fn count_fail(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
    stage: &str,
) -> Result<u32, DbError> {
    Ok(loop_gate_decision::Entity::find()
        .filter(loop_gate_decision::Column::IssueId.eq(issue_id))
        .filter(loop_gate_decision::Column::Stage.eq(stage))
        .filter(loop_gate_decision::Column::Outcome.eq(GateOutcome::Fail))
        .count(conn)
        .await? as u32)
}

pub async fn list_for_issue(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
) -> Result<Vec<LoopGateDecisionRow>, DbError> {
    Ok(loop_gate_decision::Entity::find()
        .filter(loop_gate_decision::Column::IssueId.eq(issue_id))
        .all(conn)
        .await?
        .into_iter()
        .map(to_decision_row)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::service::loop_service::{artifact, issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::loops::IssueConfig;

    fn chk(id: i32, criterion: i32, iteration: i32, scope: i32, v: CheckVerdict) -> LoopCriterionCheckRow {
        LoopCriterionCheckRow {
            id,
            criterion_id: criterion,
            iteration_id: iteration,
            scope_artifact_id: scope,
            verdict: v,
            evidence: String::new(),
        }
    }

    #[test]
    fn digest_is_order_independent() {
        let a = vec![
            chk(1, 10, 100, 5, CheckVerdict::Pass),
            chk(2, 11, 101, 5, CheckVerdict::Fail),
        ];
        let b = vec![
            chk(2, 11, 101, 5, CheckVerdict::Fail),
            chk(1, 10, 100, 5, CheckVerdict::Pass),
        ];
        assert_eq!(
            canonical_digest(&a, &[11, 10], "p"),
            canonical_digest(&b, &[10, 11], "p"),
            "digest ignores check + injected-id ordering"
        );
        assert_ne!(
            canonical_digest(&a, &[10, 11], "p"),
            canonical_digest(&a, &[10, 11], "p2"),
            "policy participates in the digest"
        );
    }

    #[tokio::test]
    async fn record_decision_idempotent_then_conflict() {
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
        let target = artifact::create_artifact(
            &db.conn,
            sp.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::InProgress,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let checks = vec![chk(1, 10, 100, target.id, CheckVerdict::Pass)];

        let first = record_decision(
            &db.conn, sp.id, iss.row.id, target.id, "review", 0, &checks, &[10], "{}", GateOutcome::Pass,
        )
        .await
        .unwrap();
        assert!(matches!(first, RecordedDecision::Settled(_)));

        // Same inputs → idempotent Settled (no second row).
        let again = record_decision(
            &db.conn, sp.id, iss.row.id, target.id, "review", 0, &checks, &[10], "{}", GateOutcome::Pass,
        )
        .await
        .unwrap();
        assert!(matches!(again, RecordedDecision::Settled(_)));
        assert_eq!(count_fail(&db.conn, iss.row.id, "review").await.unwrap(), 0);
        assert_eq!(
            outcome_for(&db.conn, target.id, "review", 0).await.unwrap(),
            Some(GateOutcome::Pass)
        );

        // Different inputs at the same key → Conflict (existing kept).
        let diverged = vec![chk(2, 10, 100, target.id, CheckVerdict::Fail)];
        let conflict = record_decision(
            &db.conn, sp.id, iss.row.id, target.id, "review", 0, &diverged, &[10], "{}", GateOutcome::Fail,
        )
        .await
        .unwrap();
        assert!(matches!(conflict, RecordedDecision::Conflict(_)));
        assert_eq!(
            outcome_for(&db.conn, target.id, "review", 0).await.unwrap(),
            Some(GateOutcome::Pass),
            "the original decision is never overwritten"
        );
    }
}
