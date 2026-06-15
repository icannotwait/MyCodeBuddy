//! Parallel result-stage fan-in (spec §4.4): atomically integrate a parallel
//! issue's frozen per-task commits onto its issue branch, then synthesize the
//! result.
//!
//! The shape is a **deferred, recoverable, atomic** integration:
//! 1. Claim a write-once session lock — the versioned `fan_in_manifest`
//!    (`{v, issue_base_oid, ordered:[{task_id, sha}]}`), distinct from the
//!    in-flight-agent lease. `ordered` freezes the topological merge order so a
//!    resume replays it rather than recomputing from mutable DB state.
//! 2. Merge each frozen task commit into a temp `integrate` worktree/branch
//!    ([`worktree::fan_in_tasks`]) — resumable (already-merged commits skip),
//!    conflict-aware (a conflict is handed to a result-stage agent that resolves
//!    it and `git commit`s).
//! 3. CAS-land the integrate tip onto the issue branch
//!    ([`worktree::cas_advance_branch`]) — atomic w.r.t. the issue branch, so a
//!    crash mid-fan-in leaves the issue branch untouched (the integrate branch is
//!    discardable). Only AFTER landing is the result artifact synthesized, so a
//!    failed land never strands a result row blocking retry.
//!
//! Serial issues never enter here — they keep the agent-submitted finalize path.

use std::path::Path;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_artifact_revision::ActorKind;
use crate::db::entities::loop_inbox_item::InboxKind;
use crate::db::entities::loop_issue::{self, IssueStatus};
use crate::db::entities::loop_iteration::{self, IterationStatus, Stage};
use crate::db::entities::loop_link::LinkKind;
use crate::db::service::{folder_service, loop_service};
use crate::db::AppDatabase;
use crate::models::loops::{IssueConfig, LoopArtifactRow, LoopDagView};
use crate::web::event_bridge::EventEmitter;

use crate::loop_engine::dispatch::{
    dispatch_iteration, emit_changed, DispatchInput, LoopAgentSpawner,
};
use crate::loop_engine::driver::resolve_agent_spec;
use crate::loop_engine::error::LoopError;
use crate::loop_engine::gates::StepOutcome;
use crate::loop_engine::transitions::{cas_issue_status, clear_fan_in, try_claim_fan_in};
use crate::loop_engine::worktree::{self, FanInOutcome};

/// One frozen task commit in the fan-in manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FanInEntry {
    task_id: i32,
    sha: String,
}

/// Versioned, write-once fan-in session manifest. `ordered` is the topological
/// merge order frozen at claim time — a resume replays it verbatim, never
/// recomputing from (mutable) DB state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FanInManifest {
    v: u32,
    /// The issue branch tip at claim time — the integrate branch's base AND the
    /// CAS `expected_old` for the landing.
    issue_base_oid: String,
    ordered: Vec<FanInEntry>,
}

impl FanInManifest {
    fn ordered_pairs(&self) -> Vec<(i32, String)> {
        self.ordered
            .iter()
            .map(|e| (e.task_id, e.sha.clone()))
            .collect()
    }
}

fn parse_manifest(json: &str) -> Result<FanInManifest, LoopError> {
    serde_json::from_str(json)
        .map_err(|e| LoopError::InvalidInput(format!("fan-in manifest decode: {e}")))
}

/// Drive a parallel issue's result-stage fan-in for one tick. Returns
/// [`StepOutcome`] like the gates: `Dispatched` (a conflict resolver is in
/// flight), `Advanced` (durable progress — landed / blocked / restarted; re-tick),
/// or `Idle` (waiting on in-flight work). Called from [`super::gates::run_finalize`]
/// only when the issue is `parallel` and its result does not yet exist.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_parallel_finalize(
    db: &AppDatabase,
    data_dir: &Path,
    spawner: &dyn LoopAgentSpawner,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    dag: &LoopDagView,
    config: &IssueConfig,
    issue_worktree_folder_id: i32,
) -> Result<StepOutcome, LoopError> {
    let conn = &db.conn;

    // Wait while any iteration is in flight (a conflict resolver, or stray work)
    // — never reset/re-merge under a live agent.
    if issue_has_inflight(db, issue.id).await? {
        return Ok(StepOutcome::Idle);
    }

    let space = loop_service::space::get_space(conn, issue.space_id)
        .await?
        .ok_or_else(|| LoopError::NotFound(format!("space {}", issue.space_id)))?;
    let repo = folder_service::get_folder_by_id(conn, space.folder_id)
        .await?
        .ok_or(LoopError::Detached)?;
    let repo_path = Path::new(&repo.path);
    let issue_branch = format!("loop/{}/issue-{}", issue.space_id, issue.seq_no);

    // Claim or adopt the fan-in manifest (write-once session lock).
    let manifest = match &issue.fan_in_manifest {
        Some(j) => parse_manifest(j)?,
        None => {
            let m = build_manifest(db, dag, issue_worktree_folder_id).await?;
            let json = serde_json::to_string(&m)
                .map_err(|e| LoopError::InvalidInput(format!("fan-in manifest encode: {e}")))?;
            if try_claim_fan_in(conn, issue.id, &json).await? {
                m
            } else {
                let fresh = loop_issue::Entity::find_by_id(issue.id)
                    .one(conn)
                    .await?
                    .and_then(|i| i.fan_in_manifest)
                    .ok_or_else(|| {
                        LoopError::Git("fan-in manifest vanished after a lost claim".into())
                    })?;
                parse_manifest(&fresh)?
            }
        }
    };

    // Ensure the integrate worktree (attach-first preserves in-progress merges).
    let integrate = worktree::ensure_integrate_worktree(
        conn,
        data_dir,
        issue.id,
        &manifest.issue_base_oid,
    )
    .await?;
    let integrate_path = integrate.worktree_path.clone();

    // A merge left mid-flight with NO resolver running (we passed the in-flight
    // gate) means the prior resolver ended without completing it → stuck conflict.
    if worktree::integrate_in_progress(&integrate_path).await {
        return block_fan_in(
            db,
            emitter,
            issue,
            "fan_in_conflict_unresolved",
            "a fan-in merge conflict was left unresolved by the result-stage agent",
        )
        .await;
    }
    // Clear any stray uncommitted state (committed merges are preserved by HEAD).
    worktree::reset_to_head(&integrate_path).await?;

    match worktree::fan_in_tasks(
        &integrate_path,
        &manifest.ordered_pairs(),
        &config.validation_commands,
        config.iteration_timeout_secs,
    )
    .await?
    {
        FanInOutcome::Conflict { .. } => {
            // Hand the in-progress merge to a result-stage agent that resolves it
            // and `git commit`s (working in the integrate worktree).
            let dispatched = dispatch_conflict_resolver(
                db,
                data_dir,
                spawner,
                emitter,
                issue,
                config,
                integrate.worktree_folder_id,
            )
            .await?;
            Ok(if dispatched {
                StepOutcome::Dispatched
            } else {
                StepOutcome::Idle
            })
        }
        FanInOutcome::RevalidationFailed { .. } => {
            block_fan_in(
                db,
                emitter,
                issue,
                "fan_in_revalidation_failed",
                "the integrated tree failed re-validation; the task combination broke",
            )
            .await
        }
        FanInOutcome::Integrated { tip } => {
            land_integration(
                db,
                emitter,
                issue,
                dag,
                repo_path,
                &integrate_path,
                &issue_branch,
                issue_worktree_folder_id,
                &manifest.issue_base_oid,
                &tip,
            )
            .await
        }
    }
}

/// CAS-land the integrate tip onto the issue branch, sync the issue worktree,
/// synthesize the result (after landing), then tear down the session.
#[allow(clippy::too_many_arguments)]
async fn land_integration(
    db: &AppDatabase,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    dag: &LoopDagView,
    repo_path: &Path,
    integrate_path: &Path,
    issue_branch: &str,
    issue_worktree_folder_id: i32,
    base_oid: &str,
    tip: &str,
) -> Result<StepOutcome, LoopError> {
    let conn = &db.conn;

    if !worktree::cas_advance_branch(repo_path, issue_branch, tip, base_oid).await? {
        // Lost CAS (the issue branch moved under us) → discard the integration,
        // clear the session, and restart fresh next tick.
        cleanup_integrate(issue, repo_path, integrate_path).await;
        clear_fan_in(conn, issue.id).await?;
        emit_changed(emitter, issue.space_id, issue.id, issue.id, "iteration");
        return Ok(StepOutcome::Advanced);
    }

    // `update-ref` advanced the branch but not the issue worktree's tree — sync it
    // (its HEAD now resolves to `tip`, so reset-to-HEAD lands the files).
    if let Some(folder) = folder_service::get_folder_by_id(conn, issue_worktree_folder_id).await? {
        let p = Path::new(&folder.path);
        if p.exists() {
            worktree::reset_to_head(p).await?;
        }
    }

    // Produce the result AFTER landing — a failed land never strands a result row.
    create_result_artifact(conn, issue, dag).await?;

    clear_fan_in(conn, issue.id).await?;
    cleanup_integrate(issue, repo_path, integrate_path).await;
    emit_changed(emitter, issue.space_id, issue.id, issue.id, "iteration");
    // Result now exists → re-tick: run_finalize's shared tail opens the merge gate.
    Ok(StepOutcome::Advanced)
}

/// Build the manifest from the current Done-task set. `issue_base_oid` is the
/// issue branch tip (CAS `expected_old`); `ordered` is the Done tasks by
/// `(sort, id)` with their frozen commits.
async fn build_manifest(
    db: &AppDatabase,
    dag: &LoopDagView,
    issue_worktree_folder_id: i32,
) -> Result<FanInManifest, LoopError> {
    let folder = folder_service::get_folder_by_id(&db.conn, issue_worktree_folder_id)
        .await?
        .ok_or_else(|| LoopError::NotFound(format!("worktree folder {issue_worktree_folder_id}")))?;
    let issue_base_oid = worktree::head_commit(Path::new(&folder.path)).await?;

    let mut tasks: Vec<&LoopArtifactRow> = dag
        .artifacts
        .iter()
        .filter(|a| a.kind == ArtifactKind::Task && a.status == ArtifactStatus::Done)
        .collect();
    tasks.sort_by(|a, b| a.sort.cmp(&b.sort).then(a.id.cmp(&b.id)));

    let mut ordered = Vec::with_capacity(tasks.len());
    for t in tasks {
        // `fan_in_commit` lives on the raw row, not the DAG DTO.
        let row = loop_artifact::Entity::find_by_id(t.id)
            .one(&db.conn)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("task {}", t.id)))?;
        let sha = row.fan_in_commit.ok_or_else(|| {
            LoopError::Git(format!("done task {} has no frozen commit (invariant)", t.id))
        })?;
        ordered.push(FanInEntry { task_id: t.id, sha });
    }
    Ok(FanInManifest {
        v: 1,
        issue_base_oid,
        ordered,
    })
}

/// Engine-synthesized result capstone (parallel mode produces no agent-submitted
/// result). Idempotent — `uniq_result_per_issue` + a check guard — and fans
/// `ResultsFrom` edges to every task, mirroring the serial finalize's result.
async fn create_result_artifact(
    conn: &sea_orm::DatabaseConnection,
    issue: &loop_issue::Model,
    dag: &LoopDagView,
) -> Result<(), LoopError> {
    if loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue.id))
        .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Result))
        .one(conn)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let tasks: Vec<i32> = dag
        .artifacts
        .iter()
        .filter(|a| a.kind == ArtifactKind::Task)
        .map(|a| a.id)
        .collect();
    let summary = format!(
        "Integrated {} parallel task(s) into the issue branch.",
        tasks.len()
    );
    let art = loop_service::artifact::create_artifact(
        conn,
        issue.space_id,
        issue.id,
        ArtifactKind::Result,
        "Result",
        ArtifactStatus::Done,
        ActorKind::Agent,
        None,
    )
    .await?;
    loop_service::artifact::add_revision(conn, art.id, &summary, ActorKind::Agent, None).await?;
    for task_id in tasks {
        loop_service::link::create_link(conn, issue.space_id, art.id, task_id, LinkKind::ResultsFrom)
            .await?;
    }
    Ok(())
}

/// Dispatch a result-stage agent to resolve the in-progress fan-in merge. It runs
/// in the **integrate** worktree (so its `git commit` completes the merge there);
/// its briefing (parallel finalize) tells it to resolve conflicts and commit.
async fn dispatch_conflict_resolver(
    db: &AppDatabase,
    data_dir: &Path,
    spawner: &dyn LoopAgentSpawner,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    config: &IssueConfig,
    integrate_worktree_folder_id: i32,
) -> Result<bool, LoopError> {
    let spec = resolve_agent_spec(config, Stage::Finalize);
    let handle = dispatch_iteration(
        db,
        data_dir,
        spawner,
        emitter.clone(),
        DispatchInput {
            space_id: issue.space_id,
            issue_id: issue.id,
            stage: Stage::Finalize,
            target_artifact_id: None,
            slot_no: None,
            attempt: 0,
            agent_type: spec.agent,
            mode_id: spec.mode_id,
            config_values: spec.config_values,
            worktree_folder_id: integrate_worktree_folder_id,
        },
    )
    .await?;
    Ok(handle.is_some())
}

/// Block the issue on a structural fan-in fault (unresolved conflict / failed
/// re-validation) with a deduped inbox card, and report `Advanced` so the driver
/// re-ticks and stops on the now-blocked issue.
async fn block_fan_in(
    db: &AppDatabase,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    subject_prefix: &str,
    reason: &str,
) -> Result<StepOutcome, LoopError> {
    cas_issue_status(&db.conn, issue.id, IssueStatus::Running, IssueStatus::Blocked).await?;
    loop_service::inbox::upsert_inbox(
        &db.conn,
        issue.space_id,
        issue.id,
        None,
        InboxKind::Blocked,
        &format!("{subject_prefix}:{}", issue.id),
        serde_json::json!({ "v": 1, "reason": reason }),
    )
    .await?;
    emit_changed(emitter, issue.space_id, issue.id, issue.id, "blocked");
    Ok(StepOutcome::Advanced)
}

/// Remove the integrate worktree + force-delete its branch (best-effort — the
/// create path reconciles any leftover).
async fn cleanup_integrate(issue: &loop_issue::Model, repo_path: &Path, integrate_path: &Path) {
    let _ = worktree::remove_worktree(repo_path, integrate_path).await;
    let branch = format!("loop/{}/issue-{}-integrate", issue.space_id, issue.seq_no);
    let _ = worktree::delete_branch(repo_path, &branch, true).await;
}

/// Whether the issue has any queued/running iteration.
async fn issue_has_inflight(db: &AppDatabase, issue_id: i32) -> Result<bool, LoopError> {
    Ok(loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(
            loop_iteration::Column::Status
                .is_in([IterationStatus::Queued, IterationStatus::Running]),
        )
        .one(&db.conn)
        .await?
        .is_some())
}
