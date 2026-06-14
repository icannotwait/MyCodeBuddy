//! Loop engineering engine: drives each running issue through triage → refine →
//! design → plan → implement → verify → review → finalize, autonomously.
//!
//! The engine holds cheap clones of the shared runtime handles (database,
//! connection manager, event emitter) plus a per-issue driver registry. A
//! single instance lives per process — desktop manages it as Tauri state, the
//! web/server `AppState` reuses (or builds) the same `Arc<LoopEngine>` — so a
//! trigger from either entry point drives the same drivers.
//!
//! State is DB-authoritative (§4.1a dispatch leases); the in-process registry
//! below is only a single-instance guard, never the concurrency authority.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::AbortHandle;

use crate::acp::internal_bus::InternalEventBus;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::AcpEvent;
use crate::db::entities::loop_issue::{self, IssueStatus};
use crate::db::entities::loop_iteration::{self, IterationStatus};
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;

pub mod actions;
pub mod briefing;
pub mod config_resolver;
pub mod dispatch;
pub mod driver;
pub mod error;
pub mod gates;
pub mod ingest;
pub mod questions;
pub mod recovery;
pub mod transitions;
pub mod validation;
pub mod worktree;

pub use error::LoopError;

/// Registry entry for a running per-issue driver task. `abort` tears the task
/// down on stop/cancel; `wake` nudges it to re-tick without polling.
pub struct DriverHandle {
    pub abort: AbortHandle,
    pub wake: Arc<Notify>,
}

/// The loop engineering engine. Cheaply shareable via `Arc`; all fields are
/// either `Arc`-backed handles or cloned connection refs.
pub struct LoopEngine {
    // Read by dispatch / driver / recovery / worktree.
    db: AppDatabase,
    manager: ConnectionManager,
    data_dir: PathBuf,
    emitter: EventEmitter,
    /// Process-internal single-instance guard: at most one driver task per
    /// issue. NOT the concurrency authority — that is the DB dispatch lease.
    drivers: Mutex<HashMap<i32, DriverHandle>>,
    /// Per-repo merge serialization: two issues that share a base repo must not
    /// run their `--no-ff` landings concurrently (they would race on the base
    /// branch ref and working tree). Keyed by repo path; entries are created on
    /// demand and never removed (bounded by the number of distinct repos).
    merge_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl LoopEngine {
    pub fn new(
        db: AppDatabase,
        manager: ConnectionManager,
        data_dir: PathBuf,
        emitter: EventEmitter,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            manager,
            data_dir,
            emitter,
            drivers: Mutex::new(HashMap::new()),
            merge_locks: Mutex::new(HashMap::new()),
        })
    }

    /// The merge lock for `repo_path`, created on first use. Held across an
    /// issue's entire merge so concurrent merges into the same repo serialize.
    pub(crate) async fn repo_merge_lock(&self, repo_path: &Path) -> Arc<Mutex<()>> {
        let mut locks = self.merge_locks.lock().await;
        Arc::clone(
            locks
                .entry(repo_path.to_path_buf())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Ensure a live driver backs `issue_id`, holding the registry lock across the
    /// whole check-evict-spawn-register so no concurrent path can interleave
    /// (§2.5): a finished handle is evicted and replaced; a live one is left
    /// untouched. The single atomic owner for both external triggers and the
    /// periodic supervisor backstop — so a dead handle self-heals on *any* start
    /// path, not only via the 15s supervisor.
    pub async fn start_issue(self: &Arc<Self>, issue_id: i32) {
        let mut drivers = self.drivers.lock().await;
        match drivers.get(&issue_id) {
            Some(h) if !h.abort.is_finished() => return, // live driver already on it
            _ => {}                                       // absent or finished → (re)spawn
        }
        self.spawn_driver_into(&mut drivers, issue_id);
    }

    /// Spawn a per-issue driver task and register its handle in the held
    /// `drivers` map. The caller MUST hold the `drivers` lock (passed in by
    /// `&mut`), so the check-and-spawn stays atomic against a concurrent start.
    fn spawn_driver_into(self: &Arc<Self>, drivers: &mut HashMap<i32, DriverHandle>, issue_id: i32) {
        let wake = Arc::new(Notify::new());
        let engine = Arc::clone(self);
        let wake_for_task = Arc::clone(&wake);
        let join = tokio::spawn(async move {
            driver::run_driver(engine, issue_id, wake_for_task).await;
        });
        drivers.insert(
            issue_id,
            DriverHandle {
                abort: join.abort_handle(),
                wake,
            },
        );
    }

    /// Wake a running driver to re-tick after an iteration settles or a human
    /// action lands. No-op when the issue has no driver. `notify_one` buffers a
    /// permit, so a wake that races ahead of the driver's `notified().await` is
    /// not lost.
    pub async fn wake(&self, issue_id: i32) {
        let drivers = self.drivers.lock().await;
        if let Some(handle) = drivers.get(&issue_id) {
            handle.wake.notify_one();
        }
    }

    /// Stop a running driver and drop its registry entry.
    pub async fn stop_issue(&self, issue_id: i32) {
        let mut drivers = self.drivers.lock().await;
        if let Some(handle) = drivers.remove(&issue_id) {
            handle.abort.abort();
        }
    }

    /// Remove a driver's registry entry. Called by the driver task itself when
    /// it exits cleanly (issue left `running`); idempotent with `stop_issue`.
    pub(crate) async fn deregister_driver(&self, issue_id: i32) {
        self.drivers.lock().await.remove(&issue_id);
    }

    /// On boot, reconcile interrupted iterations and restart a driver for every
    /// still-`running` issue. Idempotent — safe on every process start,
    /// including a clean boot with nothing in flight. Reconciliation (releasing
    /// stale leases + restoring worktrees) is pure DB+git and lives in
    /// [`recovery`]; this wrapper only restarts the drivers it identifies.
    pub async fn recover_on_boot(self: &Arc<Self>) {
        match recovery::reconcile_on_boot(&self.db).await {
            Ok(running_ids) => {
                for issue_id in running_ids {
                    self.start_issue(issue_id).await;
                }
            }
            Err(e) => tracing::error!(error = %e, "recover_on_boot failed"),
        }
    }

    /// Self-healing backstop: ensure every `running` issue has a live driver,
    /// respawning any whose task is missing or has died (a per-issue driver that
    /// panics out of its loop vanishes, but the issue stays `running` in the DB —
    /// without this it would silently never advance again until the next boot).
    /// Idempotent: `start_issue` is the single-instance guard, so this is safe to
    /// call on a schedule. A finished-but-still-registered handle is dropped under
    /// the same lock that observes it, so a concurrent legitimate (re)start can
    /// never have its live handle evicted here.
    pub async fn supervise_drivers(self: &Arc<Self>) {
        let running = match loop_issue::Entity::find()
            .filter(loop_issue::Column::Status.eq(IssueStatus::Running))
            .all(&self.db.conn)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "supervisor: listing running issues failed");
                return;
            }
        };
        for issue in running {
            // Same single-lock ensure as the trigger path: check-evict-spawn under
            // one acquisition, so a finished handle is replaced and a live one is
            // never evicted out from under a concurrent legitimate start.
            let mut drivers = self.drivers.lock().await;
            match drivers.get(&issue.id) {
                Some(h) if !h.abort.is_finished() => {} // a live driver is already on it
                _ => {
                    tracing::warn!(
                        issue_id = issue.id,
                        "supervisor: (re)spawning driver for running issue"
                    );
                    self.spawn_driver_into(&mut drivers, issue.id);
                }
            }
        }
    }

    /// The periodic supervisor loop, spawned once at boot alongside the completion
    /// watcher (same `subscribe-before-spawn` pattern is unneeded — it is a poll,
    /// not an event subscriber). The 15s cadence is a heartbeat for a rare failure
    /// (driver panic), not a cap on work. The first tick fires immediately, which
    /// is a harmless idempotent pass right after `recover_on_boot`.
    pub fn supervisor_task(self: &Arc<Self>) -> impl Future<Output = ()> + Send + 'static {
        let engine = Arc::clone(self);
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                engine.supervise_drivers().await;
            }
        }
    }

    /// Reconcile every issue that currently has an in-flight iteration. Called
    /// when the completion watcher detects it dropped events (broadcast lag): a
    /// precise, immediate backstop so a missed `TurnComplete` still settles
    /// promptly instead of waiting for the next per-issue heartbeat. Idempotent
    /// (settles are CAS).
    pub(crate) async fn reconcile_all_inflight(self: &Arc<Self>) {
        let issue_ids = match loop_iteration::Entity::find()
            .filter(loop_iteration::Column::Status.eq(IterationStatus::Running))
            .select_only()
            .column(loop_iteration::Column::IssueId)
            .distinct()
            .into_tuple::<i32>()
            .all(&self.db.conn)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "lag sweep: listing in-flight issues failed");
                return;
            }
        };
        for issue_id in issue_ids {
            if let Err(e) =
                driver::reconcile_orphaned_iterations(&self.db, &self.emitter, &self.manager, issue_id)
                    .await
            {
                tracing::warn!(issue_id, error = %e, "lag sweep: reconcile failed");
            }
        }
    }

    /// Subscribe to the in-process event bus synchronously and return the
    /// completion-watcher loop future; the caller spawns it with the
    /// mode-appropriate spawner (`tauri::async_runtime::spawn` from the desktop
    /// `setup` hook, which runs outside any tokio runtime, or `tokio::spawn`
    /// under the server runtime). This is the engine's completion-awareness: a
    /// separate, additive bus subscriber (it never touches the delegation
    /// lifecycle path), settling + waking loop iterations as their turns
    /// complete, reacting only to loop conversations.
    ///
    /// `subscribe()` runs here, before the future is returned, so a
    /// `TurnComplete` emitted between this call and the first poll is buffered
    /// by the broadcast channel rather than dropped (subscribe-before-spawn).
    pub fn completion_watcher_task(
        self: &Arc<Self>,
        bus: Arc<InternalEventBus>,
    ) -> impl Future<Output = ()> + Send + 'static {
        let engine = Arc::clone(self);
        let mut rx = bus.subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(envelope) => match &envelope.payload {
                        AcpEvent::TurnComplete { .. } => {
                            engine.on_turn_complete(&envelope.connection_id).await;
                        }
                        // A loop iteration's agent asked the operator a question:
                        // surface it as a `question` inbox card. Cleared on the
                        // matching QuestionResolved.
                        AcpEvent::QuestionRequest {
                            question_id,
                            questions,
                        } => {
                            engine
                                .on_question_request(
                                    &envelope.connection_id,
                                    question_id,
                                    questions,
                                )
                                .await;
                        }
                        AcpEvent::QuestionResolved { question_id } => {
                            engine.on_question_resolved(question_id).await;
                        }
                        _ => {}
                    },
                    // Dropped `n` in-process events — a `TurnComplete` may be
                    // among them. Run an immediate, precise reconcile of all
                    // in-flight iterations so settlement stays timely without
                    // waiting for the per-issue heartbeat cadence (§2.1).
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            dropped = n,
                            "completion watcher lagged; running in-flight reconcile sweep"
                        );
                        engine.reconcile_all_inflight().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    /// Settle the loop iteration backing a just-completed connection's turn,
    /// then wake its issue driver to advance the DAG. No-op for any connection
    /// that isn't a running loop iteration (e.g. ordinary or delegation turns).
    pub async fn on_turn_complete(self: &Arc<Self>, connection_id: &str) {
        // Resolve the conversation backing this connection (in-memory, same as
        // the delegation lifecycle path).
        let Some((state, _)) = self.manager.get_state_and_emitter(connection_id).await else {
            // Connection state already gone (e.g. teardown raced this event); the
            // driver's periodic reconcile will settle the iteration instead.
            tracing::debug!(connection_id, "turn complete: no connection state (already torn down?)");
            return;
        };
        let conversation_id = state.read().await.conversation_id;
        let Some(cid) = conversation_id else {
            tracing::debug!(connection_id, "turn complete: connection has no conversation_id");
            return;
        };
        // DB-authoritative: is this conversation a running loop iteration?
        let iter = match loop_iteration::Entity::find()
            .filter(loop_iteration::Column::ConversationId.eq(cid))
            .filter(loop_iteration::Column::Status.eq(IterationStatus::Running))
            .one(&self.db.conn)
            .await
        {
            Ok(Some(it)) => it,
            // Not a running loop iteration (ordinary/delegation turn, or already
            // settled by the reconcile) — nothing to do.
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "on_turn_complete: iteration lookup failed");
                return;
            }
        };
        if let Err(e) = self.settle_iteration(iter.id).await {
            tracing::warn!(iteration_id = iter.id, error = %e, "settle iteration failed");
        } else {
            tracing::debug!(
                iteration_id = iter.id,
                issue_id = iter.issue_id,
                "settled iteration on turn complete"
            );
        }
        self.wake(iter.issue_id).await;
    }

    /// Run the §4.3 seven-step dispatch for a single frontier decision. Returns
    /// `Ok(None)` when the dispatch lease was already held (lost the race). The
    /// driver (Task 1.6) chooses the [`dispatch::DispatchInput`]; this just
    /// executes it against the live connection manager.
    pub async fn dispatch_iteration(
        &self,
        input: dispatch::DispatchInput,
    ) -> Result<Option<dispatch::DispatchHandle>, LoopError> {
        dispatch::dispatch_iteration(
            &self.db,
            &self.data_dir,
            &self.manager,
            self.emitter.clone(),
            input,
        )
        .await
    }

    /// §4.9 settlement for a finished iteration (token accounting + success CAS
    /// + no-progress signal).
    pub async fn settle_iteration(
        &self,
        iteration_id: i32,
    ) -> Result<dispatch::SettleOutcome, LoopError> {
        dispatch::settle_iteration(&self.db, &self.emitter, iteration_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::service::loop_service::{issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::cas_issue_status;
    use crate::models::loops::IssueConfig;

    #[tokio::test]
    async fn supervisor_respawns_dead_driver_for_running_issue() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/loop-supervisor").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        cas_issue_status(&db.conn, issue.row.id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            std::path::PathBuf::from("/tmp/loop-supervisor-data"),
            EventEmitter::Noop,
        );

        // No driver registered — simulates one that panicked out / never started.
        assert!(engine.drivers.lock().await.get(&issue.row.id).is_none());

        engine.supervise_drivers().await;

        // A live driver now backs the still-running issue (it idles harmlessly:
        // the issue has no worktree folder in this test, so the tick parks).
        let drivers = engine.drivers.lock().await;
        let handle = drivers.get(&issue.row.id).expect("driver respawned");
        assert!(!handle.abort.is_finished(), "respawned driver is live");
    }

    #[tokio::test]
    async fn supervisor_leaves_live_driver_untouched() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/loop-supervisor2").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        cas_issue_status(&db.conn, issue.row.id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            std::path::PathBuf::from("/tmp/loop-supervisor2-data"),
            EventEmitter::Noop,
        );

        engine.start_issue(issue.row.id).await;
        let first = engine
            .drivers
            .lock()
            .await
            .get(&issue.row.id)
            .map(|h| h.abort.id());

        // A second supervision pass must not replace the existing live driver.
        engine.supervise_drivers().await;
        let second = engine
            .drivers
            .lock()
            .await
            .get(&issue.row.id)
            .map(|h| h.abort.id());
        assert_eq!(first, second, "live driver is not respawned");
    }

    #[tokio::test]
    async fn start_issue_replaces_a_finished_handle() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/loop-ensure").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        cas_issue_status(&db.conn, issue.row.id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            std::path::PathBuf::from("/tmp/loop-ensure-data"),
            EventEmitter::Noop,
        );

        // Register a *finished* handle (simulates a driver that returned/panicked
        // but whose registry entry was never evicted).
        let done = tokio::spawn(async {});
        let abort = done.abort_handle();
        done.await.unwrap();
        assert!(abort.is_finished());
        engine.drivers.lock().await.insert(
            issue.row.id,
            DriverHandle {
                abort,
                wake: Arc::new(Notify::new()),
            },
        );

        // A direct start must evict the finished handle and spawn a live one (the
        // old `contains_key` guard would have no-opped on the stale entry).
        engine.start_issue(issue.row.id).await;
        let drivers = engine.drivers.lock().await;
        let handle = drivers.get(&issue.row.id).expect("driver present");
        assert!(
            !handle.abort.is_finished(),
            "finished handle replaced by a live driver"
        );
    }
}
