//! The single source of every criterion *handle* a review/integration gate
//! injects and resolves against (spec §3.4/§3.6, plan D9/D10).
//!
//! A handle is a stable ordinal string — never a DB id — so the agent trust
//! boundary is preserved (ingest resolves ordinals, never ids). Three maps:
//!
//!   • [`task_review_ordinals`] — what a TASK review must check: the acceptance
//!     criteria the task `covers` (`R{i}.AC{j}`) plus the task's own acceptance
//!     (`T{n}`); a task that covers nothing and has no own acceptance falls back
//!     to all requirement acceptance (D11, never vacuous).
//!   • [`integration_ordinals`] — what an INTEGRATION review (target = result)
//!     must check: all requirement acceptance (`R{i}.AC{j}`) plus all design
//!     obligations (`D{k}`); on the `direct` route (no requirements) it degrades
//!     to the tasks' own acceptance (`T{n}.AC{j}`).
//!   • [`obligation_ordinals`] — the `D{k}` design-obligation map, shown in a
//!     task briefing as awareness-only context.
//!
//! The acceptance handles are built on top of the SAME ordering as
//! [`super::coverage::acceptance_ordinals_for_issue`], so `R1.AC1` means the same
//! criterion here as in `covers`, the coverage gate, and the planner briefing.
//! The map is persisted into the iteration's `context_manifest` at dispatch and
//! ingest resolves submitted handles against that stored copy, so a concurrent
//! replan can never drift the handles a reviewer was shown.

use std::collections::{HashMap, HashSet};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_coverage;
use crate::db::entities::loop_criterion::{self, CriterionKind};
use crate::db::error::DbError;

/// One injectable criterion: its stable handle, the DB id it resolves to, and
/// the text + kind for rendering the briefing checklist.
#[derive(Debug, Clone)]
pub struct OrdinalEntry {
    pub handle: String,
    pub criterion_id: i32,
    pub text: String,
    pub kind: CriterionKind,
}

/// `criterion_id → text` for a set of ids, in one query.
async fn criterion_texts(
    conn: &impl sea_orm::ConnectionTrait,
    ids: &[i32],
) -> Result<HashMap<i32, String>, DbError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(loop_criterion::Entity::find()
        .filter(loop_criterion::Column::Id.is_in(ids.to_vec()))
        .all(conn)
        .await?
        .into_iter()
        .map(|c| (c.id, c.text))
        .collect())
}

/// `R{i}.AC{j}` entries for the issue's live done requirements, ordered exactly
/// as [`super::coverage::acceptance_ordinals_for_issue`] (the canonical id
/// ordering) so the handles are byte-identical to `covers` / the coverage gate.
async fn requirement_acceptance_entries(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<OrdinalEntry>, DbError> {
    let ordered = super::coverage::acceptance_ordinals_for_issue(conn, issue_id).await?;
    let ids: Vec<i32> = ordered.iter().flat_map(|(_, cs)| cs.iter().copied()).collect();
    let texts = criterion_texts(conn, &ids).await?;
    let mut out = Vec::new();
    for (ri, (_req, crits)) in ordered.iter().enumerate() {
        for (ci, cid) in crits.iter().enumerate() {
            out.push(OrdinalEntry {
                handle: format!("R{}.AC{}", ri + 1, ci + 1),
                criterion_id: *cid,
                text: texts.get(cid).cloned().unwrap_or_default(),
                kind: CriterionKind::Acceptance,
            });
        }
    }
    Ok(out)
}

/// A task's own acceptance criteria as `T{n}` entries, ordered by `(sort, id)`.
async fn task_acceptance_entries(
    conn: &sea_orm::DatabaseConnection,
    task_id: i32,
) -> Result<Vec<OrdinalEntry>, DbError> {
    let crits = loop_criterion::Entity::find()
        .filter(loop_criterion::Column::ArtifactId.eq(task_id))
        .filter(loop_criterion::Column::Kind.eq(CriterionKind::Acceptance))
        .order_by_asc(loop_criterion::Column::Sort)
        .order_by_asc(loop_criterion::Column::Id)
        .all(conn)
        .await?;
    Ok(crits
        .into_iter()
        .enumerate()
        .map(|(n, c)| OrdinalEntry {
            handle: format!("T{}", n + 1),
            criterion_id: c.id,
            text: c.text,
            kind: c.kind,
        })
        .collect())
}

/// The criteria a TASK review must check (D9): the acceptance criteria the task
/// `covers` (`R{i}.AC{j}`, canonical order) plus the task's own acceptance
/// (`T{n}`). Empty-set fallback (D11): a task that covers nothing AND has no own
/// acceptance requires checks against ALL requirement acceptance — never vacuous.
pub async fn task_review_ordinals(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
    task_id: i32,
) -> Result<Vec<OrdinalEntry>, DbError> {
    let req_acc = requirement_acceptance_entries(conn, issue_id).await?;
    let covered: HashSet<i32> = loop_coverage::Entity::find()
        .filter(loop_coverage::Column::TaskArtifactId.eq(task_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|c| c.criterion_id)
        .collect();

    let mut out: Vec<OrdinalEntry> = req_acc
        .iter()
        .filter(|e| covered.contains(&e.criterion_id))
        .cloned()
        .collect();
    out.extend(task_acceptance_entries(conn, task_id).await?);

    // D11: whole task-scoped set empty → fall back to all requirement acceptance.
    if out.is_empty() && !req_acc.is_empty() {
        out = req_acc;
    }
    Ok(out)
}

/// The design obligations (`D{k}`) of the issue — `constraint | invariant |
/// obligation` criteria on live done designs, flat-numbered across designs by
/// `(design sort,id)` then `(criterion sort,id)`. Requirements never carry these
/// (P1 typed allow-set), so this is exactly the cross-cutting set.
pub async fn obligation_ordinals(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<OrdinalEntry>, DbError> {
    let designs = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue_id))
        .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Design))
        .filter(loop_artifact::Column::Status.eq(ArtifactStatus::Done))
        .order_by_asc(loop_artifact::Column::Sort)
        .order_by_asc(loop_artifact::Column::Id)
        .all(conn)
        .await?;
    let mut out = Vec::new();
    let mut k = 0;
    for d in designs {
        let crits = loop_criterion::Entity::find()
            .filter(loop_criterion::Column::ArtifactId.eq(d.id))
            .order_by_asc(loop_criterion::Column::Sort)
            .order_by_asc(loop_criterion::Column::Id)
            .all(conn)
            .await?;
        for c in crits {
            if matches!(
                c.kind,
                CriterionKind::Constraint | CriterionKind::Invariant | CriterionKind::Obligation
            ) {
                k += 1;
                out.push(OrdinalEntry {
                    handle: format!("D{k}"),
                    criterion_id: c.id,
                    text: c.text,
                    kind: c.kind,
                });
            }
        }
    }
    Ok(out)
}

/// The whole-issue closure an INTEGRATION review (target = result) must check
/// (D9): all requirement acceptance (`R{i}.AC{j}`) plus all design obligations
/// (`D{k}`). Route degradation: on `direct` (no requirements) the acceptance
/// closure becomes the live tasks' own acceptance (`T{n}.AC{j}`); `skip_design`
/// naturally yields no `D{k}`.
pub async fn integration_ordinals(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<OrdinalEntry>, DbError> {
    let mut out = requirement_acceptance_entries(conn, issue_id).await?;
    if out.is_empty() {
        // direct route: no requirements → the live tasks' own acceptance.
        let tasks = loop_artifact::Entity::find()
            .filter(loop_artifact::Column::IssueId.eq(issue_id))
            .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Task))
            .filter(
                loop_artifact::Column::Status
                    .is_not_in([ArtifactStatus::Superseded, ArtifactStatus::Cancelled]),
            )
            .order_by_asc(loop_artifact::Column::Sort)
            .order_by_asc(loop_artifact::Column::Id)
            .all(conn)
            .await?;
        for (n, t) in tasks.iter().enumerate() {
            let crits = loop_criterion::Entity::find()
                .filter(loop_criterion::Column::ArtifactId.eq(t.id))
                .filter(loop_criterion::Column::Kind.eq(CriterionKind::Acceptance))
                .order_by_asc(loop_criterion::Column::Sort)
                .order_by_asc(loop_criterion::Column::Id)
                .all(conn)
                .await?;
            for (j, c) in crits.iter().enumerate() {
                out.push(OrdinalEntry {
                    handle: format!("T{}.AC{}", n + 1, j + 1),
                    criterion_id: c.id,
                    text: c.text.clone(),
                    kind: c.kind,
                });
            }
        }
    }
    out.extend(obligation_ordinals(conn, issue_id).await?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::service::loop_service::{artifact, coverage, issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::loops::IssueConfig;

    /// Two requirements (R1: AC1; R2: AC1), one design with an invariant, two
    /// tasks (T1 covers R1.AC1 + has its own acceptance; T2 covers R2.AC1).
    /// Returns `(db, space_id, issue_id, t1, t2, design)`.
    async fn seed_full() -> (crate::db::AppDatabase, i32, i32, i32, i32, i32) {
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
        let r1 = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Requirement, "R1", ArtifactStatus::Done, ActorKind::Agent, None).await.unwrap();
        let r1ac = artifact::add_criterion(&db.conn, r1.id, CriterionKind::Acceptance, "r1 holds").await.unwrap();
        let r2 = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Requirement, "R2", ArtifactStatus::Done, ActorKind::Agent, None).await.unwrap();
        let r2ac = artifact::add_criterion(&db.conn, r2.id, CriterionKind::Acceptance, "r2 holds").await.unwrap();
        let design = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Design, "D", ArtifactStatus::Done, ActorKind::Agent, None).await.unwrap();
        artifact::add_criterion(&db.conn, design.id, CriterionKind::Invariant, "stays O(1)").await.unwrap();
        let t1 = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Task, "T1", ArtifactStatus::Pending, ActorKind::Agent, None).await.unwrap();
        artifact::add_criterion(&db.conn, t1.id, CriterionKind::Acceptance, "t1 own").await.unwrap();
        coverage::create_coverage(&db.conn, sp.id, t1.id, r1ac.id).await.unwrap();
        let t2 = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Task, "T2", ArtifactStatus::Pending, ActorKind::Agent, None).await.unwrap();
        coverage::create_coverage(&db.conn, sp.id, t2.id, r2ac.id).await.unwrap();
        (db, sp.id, iss.row.id, t1.id, t2.id, design.id)
    }

    #[tokio::test]
    async fn task_review_ordinals_cover_plus_own_acceptance() {
        let (db, _sp, issue_id, t1, t2, _d) = seed_full().await;

        // T1: covered R1.AC1 + its own T1 acceptance; NOT the unrelated R2.AC1.
        let m1 = task_review_ordinals(&db.conn, issue_id, t1).await.unwrap();
        let handles: Vec<&str> = m1.iter().map(|e| e.handle.as_str()).collect();
        assert_eq!(handles, vec!["R1.AC1", "T1"], "covered AC then own acceptance");
        assert!(!handles.contains(&"R2.AC1"), "unrelated requirement AC not injected");

        // T2: covered R2.AC1; no own acceptance.
        let m2 = task_review_ordinals(&db.conn, issue_id, t2).await.unwrap();
        assert_eq!(m2.iter().map(|e| e.handle.as_str()).collect::<Vec<_>>(), vec!["R2.AC1"]);
    }

    #[tokio::test]
    async fn task_review_empty_falls_back_to_all_requirement_acceptance() {
        let (db, sp, issue_id, _t1, _t2, _d) = seed_full().await;
        // A task that covers nothing and has no own acceptance.
        let bare = artifact::create_artifact(&db.conn, sp, issue_id, ArtifactKind::Task, "T3", ArtifactStatus::Pending, ActorKind::Agent, None).await.unwrap();
        let m = task_review_ordinals(&db.conn, issue_id, bare.id).await.unwrap();
        assert_eq!(
            m.iter().map(|e| e.handle.as_str()).collect::<Vec<_>>(),
            vec!["R1.AC1", "R2.AC1"],
            "empty closure falls back to every requirement acceptance"
        );
    }

    #[tokio::test]
    async fn integration_ordinals_is_requirements_plus_obligations() {
        let (db, _sp, issue_id, _t1, _t2, _d) = seed_full().await;
        let m = integration_ordinals(&db.conn, issue_id).await.unwrap();
        assert_eq!(
            m.iter().map(|e| e.handle.as_str()).collect::<Vec<_>>(),
            vec!["R1.AC1", "R2.AC1", "D1"],
            "whole-issue closure = all requirement acceptance + design obligations"
        );
    }

    #[tokio::test]
    async fn obligation_ordinals_only_design_cross_cutting() {
        let (db, _sp, issue_id, _t1, _t2, _d) = seed_full().await;
        let m = obligation_ordinals(&db.conn, issue_id).await.unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].handle, "D1");
        assert_eq!(m[0].kind, CriterionKind::Invariant);
    }

    #[tokio::test]
    async fn integration_direct_route_uses_task_acceptance() {
        // No requirements, no design → direct route closure = tasks' own acceptance.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo2").await;
        let sp = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(&db.conn, sp.id, "I", "b", IssuePriority::Medium, Some(&IssueConfig::default())).await.unwrap();
        let t = artifact::create_artifact(&db.conn, sp.id, iss.row.id, ArtifactKind::Task, "T1", ArtifactStatus::Pending, ActorKind::Agent, None).await.unwrap();
        artifact::add_criterion(&db.conn, t.id, CriterionKind::Acceptance, "a").await.unwrap();
        artifact::add_criterion(&db.conn, t.id, CriterionKind::Acceptance, "b").await.unwrap();
        let m = integration_ordinals(&db.conn, iss.row.id).await.unwrap();
        assert_eq!(
            m.iter().map(|e| e.handle.as_str()).collect::<Vec<_>>(),
            vec!["T1.AC1", "T1.AC2"],
            "direct route degrades to the tasks' own acceptance"
        );
    }
}
