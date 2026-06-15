//! Crash recovery: idempotent boot-time reconciliation (§4.10).
//!
//! A fresh process has no live ACP connections, so any loop iteration still in
//! an active (`queued`/`running`) status is an *interruption* — its agent is
//! gone and its turn will never complete. Recovery marks each such iteration
//! `interrupted`, which releases its §4.1a dispatch lease (the partial unique
//! indexes are predicated on `status IN ('queued','running')`). This is a pure
//! status change: `attempt` is never bumped (a pure interruption resume is not a
//! rework), and an interrupted iteration is never faked as succeeded.
//!
//! Idempotency falls out of the driver's frontier, not iteration-level dedup:
//! the frontier keys off DAG state (which artifacts exist), so a re-tick simply
//! skips any stage whose output already landed via MCP — already-persisted
//! artifacts are never produced twice. The interrupted iteration keeps its
//! backing conversation as an audit record of the partial run; it is harmless
//! (hidden by the `kind=loop` sidebar guard) and never reused.
//!
//! For every issue still `running`, recovery also restores the clean-tree
//! invariant (`reset --hard HEAD && clean -fd`), discarding only uncommitted
//! side-effects left by an interrupted implement — never rewinding a committed
//! checkpoint. In the M2.1 read pipeline the worktree is always clean, so this
//! is a no-op there; it becomes load-bearing once implement lands in M2.2.

use std::path::Path;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveEnum, ColumnTrait, EntityTrait, QueryFilter};

use crate::db::entities::loop_issue::{self, IssueStatus};
use crate::db::entities::loop_iteration::{self, IterationStatus};
use crate::db::service::{folder_service, loop_service};
use crate::db::AppDatabase;

use crate::loop_engine::error::LoopError;
use crate::loop_engine::worktree;

/// Reconcile interrupted iterations and restore running issues' worktrees.
/// Returns the ids of issues still `running`, whose drivers the engine must
/// restart. Pure over DB + git (no `ConnectionManager`), so it is unit-tested
/// directly; restarting drivers from the returned ids is the thin part left to
/// the caller ([`super::LoopEngine::recover_on_boot`]).
pub(crate) async fn reconcile_on_boot(db: &AppDatabase) -> Result<Vec<i32>, LoopError> {
    let conn = &db.conn;

    // 1. Release stale leases: every active iteration is interrupted (no live
    //    connection survives a restart). A pure status change — `attempt` is not
    //    bumped, and the run is never faked as succeeded.
    loop_iteration::Entity::update_many()
        .col_expr(
            loop_iteration::Column::Status,
            Expr::value(IterationStatus::Interrupted.to_value()),
        )
        .col_expr(loop_iteration::Column::EndedAt, Expr::value(Utc::now()))
        .filter(
            loop_iteration::Column::Status
                .is_in([IterationStatus::Queued, IterationStatus::Running]),
        )
        .exec(conn)
        .await?;

    // 2. Running issues: restore the clean-tree invariant, then hand their ids
    //    back so the caller can restart drivers.
    let running = loop_issue::Entity::find()
        .filter(loop_issue::Column::Status.eq(IssueStatus::Running))
        .all(conn)
        .await?;

    for issue in &running {
        restore_worktree_clean(db, issue).await;
    }
    Ok(running.iter().map(|i| i.id).collect())
}

/// Discard any uncommitted side-effects in an issue's worktree, returning it to
/// its branch HEAD (the latest accepted checkpoint). No-op when the issue has no
/// on-disk worktree. Best-effort: a git failure is logged, not fatal — the
/// driver still restarts, and a tree it can't clean surfaces later as a
/// no-progress signal rather than blocking boot.
async fn restore_worktree_clean(db: &AppDatabase, issue: &loop_issue::Model) {
    let Some(folder_id) = issue.worktree_folder_id else {
        return;
    };
    let folder = match folder_service::get_folder_by_id(&db.conn, folder_id).await {
        Ok(Some(f)) => f,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(folder_id, error = %e, "recover: worktree folder lookup failed");
            return;
        }
    };
    // The space repo — needed to enumerate a parallel issue's per-task /
    // integrate worktrees (resolved best-effort; absence just skips the subtree).
    let repo_path = match loop_service::space::get_space(&db.conn, issue.space_id).await {
        Ok(Some(space)) => folder_service::get_folder_by_id(&db.conn, space.folder_id)
            .await
            .ok()
            .flatten()
            .map(|repo| repo.path),
        _ => None,
    };

    let issue_wt = Path::new(&folder.path);
    if issue_wt.exists() {
        if let Err(e) = worktree::reset_to_head(issue_wt).await {
            tracing::warn!(path = %folder.path, error = %e, "recover: reset worktree failed");
        }
    }
    // Parallel issues also have per-task / integrate worktrees to restore (discard
    // crash-time uncommitted residue; committed task checkpoints survive).
    if let Some(repo) = repo_path {
        let _ = worktree::reset_issue_subtree(Path::new(&repo), issue_wt).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::Stage;
    use crate::db::service::loop_service::{artifact, issue, space};
    use crate::db::test_helpers::{fresh_disk_db, fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{
        cas_iteration_status, try_claim_iteration, IterationClaim,
    };
    use crate::loop_engine::worktree::{checkpoint, ensure_worktree};
    use crate::models::loops::IssueConfig;
    use std::process::Command as StdCommand;

    /// Mark an issue `running` (the trigger precondition recovery keys off).
    async fn set_running(db: &AppDatabase, issue_id: i32) {
        loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Running.to_value()),
            )
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&db.conn)
            .await
            .unwrap();
    }

    async fn set_status(db: &AppDatabase, issue_id: i32, status: IssueStatus) {
        loop_issue::Entity::update_many()
            .col_expr(loop_issue::Column::Status, Expr::value(status.to_value()))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&db.conn)
            .await
            .unwrap();
    }

    /// Claim a lease and (optionally) flip it to `running`, returning the row.
    async fn claim(
        db: &AppDatabase,
        space_id: i32,
        issue_id: i32,
        stage: Stage,
        target: Option<i32>,
        attempt: i32,
        run: bool,
    ) -> loop_iteration::Model {
        let iter = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id,
                issue_id,
                stage,
                target_artifact_id: target,
                slot_no: None,
                capability_token: format!("tok-{issue_id}-{stage:?}-{attempt}"),
                attempt,
            },
        )
        .await
        .unwrap()
        .expect("lease claimed");
        if run {
            cas_iteration_status(
                &db.conn,
                iter.id,
                IterationStatus::Queued,
                IterationStatus::Running,
            )
            .await
            .unwrap();
        }
        iter
    }

    async fn get_iter(db: &AppDatabase, id: i32) -> loop_iteration::Model {
        loop_iteration::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
    }

    async fn seed_issue(db: &AppDatabase, folder_id: i32) -> (i32, i32) {
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "Issue",
            "body",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        (space.id, issue.row.id)
    }

    #[tokio::test]
    async fn interrupts_active_iterations_without_bumping_attempt() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/recover-a").await;
        let (space_id, issue_id) = seed_issue(&db, folder_id).await;
        set_running(&db, issue_id).await; // worktree_folder_id stays None → reset is a no-op

        let task = artifact::create_artifact(
            &db.conn,
            space_id,
            issue_id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        // A queued triage (claimed, never ran) and a running implement (attempt 3).
        let queued = claim(&db, space_id, issue_id, Stage::Triage, None, 0, false).await;
        let running = claim(&db, space_id, issue_id, Stage::Implement, Some(task.id), 3, true).await;

        let restart = reconcile_on_boot(&db).await.unwrap();
        assert_eq!(restart, vec![issue_id], "the running issue is queued for restart");

        let q = get_iter(&db, queued.id).await;
        let r = get_iter(&db, running.id).await;
        assert_eq!(q.status, IterationStatus::Interrupted);
        assert_eq!(r.status, IterationStatus::Interrupted);
        assert!(q.ended_at.is_some() && r.ended_at.is_some(), "abandonment stamped");
        assert_eq!(r.attempt, 3, "pure interruption never bumps attempt");
    }

    #[tokio::test]
    async fn releases_lease_so_redispatch_can_reclaim() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/recover-b").await;
        let (space_id, issue_id) = seed_issue(&db, folder_id).await;
        set_running(&db, issue_id).await;

        let task = artifact::create_artifact(
            &db.conn,
            space_id,
            issue_id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        claim(&db, space_id, issue_id, Stage::Implement, Some(task.id), 0, true).await;

        // While the lease is held, a second implement on the issue is leased out.
        let blocked = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Implement,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "blocked".into(),
                attempt: 1,
            },
        )
        .await
        .unwrap();
        assert!(blocked.is_none(), "uniq_active_write holds before recovery");

        reconcile_on_boot(&db).await.unwrap();

        // The lease is now free: a fresh dispatch can reclaim it.
        let reclaimed = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Implement,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "reclaim".into(),
                attempt: 1,
            },
        )
        .await
        .unwrap();
        assert!(reclaimed.is_some(), "interruption released the dispatch lease");
    }

    #[tokio::test]
    async fn restart_list_holds_only_running_issues_but_all_leases_release() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/recover-c").await;
        let (space_id, running_issue) = seed_issue(&db, folder_id).await;
        set_running(&db, running_issue).await;
        // A paused issue with its own in-flight iteration (e.g. crashed mid-pause).
        let paused_issue = issue::create_issue(
            &db.conn,
            space_id,
            "Paused",
            "body",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap()
        .row
        .id;
        set_status(&db, paused_issue, IssueStatus::Paused).await;

        let on_running = claim(&db, space_id, running_issue, Stage::Triage, None, 0, true).await;
        let on_paused = claim(&db, space_id, paused_issue, Stage::Triage, None, 0, true).await;

        let restart = reconcile_on_boot(&db).await.unwrap();
        assert_eq!(restart, vec![running_issue], "only running issues restart");

        // Both iterations are reconciled regardless of issue status — a dead
        // connection's lease must release even for a paused issue.
        assert_eq!(
            get_iter(&db, on_running.id).await.status,
            IterationStatus::Interrupted
        );
        assert_eq!(
            get_iter(&db, on_paused.id).await.status,
            IterationStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn idempotent_and_clean_boot_is_noop() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/recover-d").await;
        let (_space_id, issue_id) = seed_issue(&db, folder_id).await;
        set_running(&db, issue_id).await;

        // No active iterations: a clean boot still lists the running issue, twice,
        // without error (idempotent).
        assert_eq!(reconcile_on_boot(&db).await.unwrap(), vec![issue_id]);
        assert_eq!(reconcile_on_boot(&db).await.unwrap(), vec![issue_id]);
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "tester"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    #[tokio::test]
    async fn resets_dirty_worktree_to_head_on_recovery() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let data = tempfile::tempdir().unwrap();
        let db = fresh_disk_db(data.path()).await;
        let folder_id = seed_folder(&db, &repo.path().to_string_lossy()).await;
        let (space_id, issue_id) = seed_issue(&db, folder_id).await;

        // Trigger: bind the worktree (ensure_worktree records it on the issue),
        // then mark the issue running and leave an interrupted iteration behind.
        let ctx = ensure_worktree(&db.conn, data.path(), issue_id)
            .await
            .unwrap();
        set_running(&db, issue_id).await;
        let iter = claim(&db, space_id, issue_id, Stage::Implement, None, 0, true).await;

        // Accept one checkpoint, then leave the tree dirty (as a crashed implement
        // would): a modified tracked file plus an untracked scratch file.
        std::fs::write(ctx.worktree_path.join("kept.txt"), "keep\n").unwrap();
        checkpoint(&ctx.worktree_path, "loop: keep")
            .await
            .unwrap()
            .expect("committed");
        std::fs::write(ctx.worktree_path.join("kept.txt"), "dirty\n").unwrap();
        std::fs::write(ctx.worktree_path.join("scratch.txt"), "temp\n").unwrap();

        let restart = reconcile_on_boot(&db).await.unwrap();
        assert_eq!(restart, vec![issue_id]);

        // The iteration is interrupted and the tree is back at the checkpoint:
        // committed work kept, uncommitted side-effects discarded.
        assert_eq!(
            get_iter(&db, iter.id).await.status,
            IterationStatus::Interrupted
        );
        assert_eq!(
            std::fs::read_to_string(ctx.worktree_path.join("kept.txt")).unwrap(),
            "keep\n",
            "committed checkpoint preserved"
        );
        assert!(
            !ctx.worktree_path.join("scratch.txt").exists(),
            "uncommitted side-effect discarded"
        );
    }
}
