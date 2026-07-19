//! Durable auto-title worker: claim → run → finalize/fail with permits and
//! cancellation. The database remains the queue; notifications are wake hints.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, feature = "test-utils"))]
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::acp::manager::ConnectionManager;
use crate::auto_title::internal_sessions::InternalAgentSessionRegistry;
use crate::auto_title::partial_source::{ManagerPartialSource, PartialAssistantTextSource};
use crate::auto_title::runner::{
    HiddenAgentRunner, ManagerTitleConnectionDriver, TitleAgentRunner,
};
use crate::auto_title::service::{
    claim_is_still_running, claim_next_ready, finalize_generated_title, list_deadline_candidates,
    promote_deadline_jobs_by_ids, record_attempt_failure, recover_interrupted_jobs,
    DeadlinePromoteParams,
};
use crate::auto_title::types::{
    AutoTitleAttempt, AutoTitleClaim, AutoTitleRunError, FailureTransition, FinalizeTitleOutcome,
};
use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
use crate::db::error::DbError;
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;
use std::path::PathBuf;

const MAX_CONCURRENT_ATTEMPTS: usize = 2;
const DEFAULT_DEADLINE: Duration = Duration::from_secs(300);
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(101);
const DEFAULT_BATCH_LIMIT: usize = 64;

/// Empty partial source for inert/test coordinators (no connection map).
struct EmptyPartialSource;

#[async_trait::async_trait]
impl PartialAssistantTextSource for EmptyPartialSource {
    async fn partials_for(&self, _conversation_ids: &[i32]) -> HashMap<i32, String> {
        HashMap::new()
    }
}

/// Process-local live coordinator used by lifecycle to wake ready jobs after
/// commit without threading the Arc through every bus worker signature.
static LIVE_COORDINATOR: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Weak<AutoTitleCoordinator>>>,
> = std::sync::OnceLock::new();

fn live_slot() -> &'static std::sync::Mutex<Option<std::sync::Weak<AutoTitleCoordinator>>> {
    LIVE_COORDINATOR.get_or_init(|| std::sync::Mutex::new(None))
}

fn register_live_coordinator(this: &Arc<AutoTitleCoordinator>) {
    if let Ok(mut slot) = live_slot().lock() {
        *slot = Some(Arc::downgrade(this));
    }
}

/// Wake the process-local title coordinator if one is running.
pub fn notify_live_coordinator_ready() {
    let Some(coord) = live_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(|w| w.upgrade()))
    else {
        return;
    };
    coord.notify_ready();
}

/// Per-conversation active attempt registration (claim-scoped cancel).
#[derive(Debug, Clone)]
struct ActiveTitleAttempt {
    attempt: i32,
    cancellation: CancellationToken,
}

/// Coordinates durable title claims, retries, recovery, and cancellation.
pub struct AutoTitleCoordinator {
    db: Arc<AppDatabase>,
    runner: Arc<dyn TitleAgentRunner>,
    emitter: EventEmitter,
    partial_source: Arc<dyn PartialAssistantTextSource>,
    deadline: Duration,
    sweep_interval: Duration,
    batch_limit: usize,
    attempts: Arc<Semaphore>,
    notify: Arc<Notify>,
    active: Mutex<HashMap<i32, ActiveTitleAttempt>>,
    off_root: Mutex<CancellationToken>,
    started: AtomicBool,
    /// True while a claim-error delayed wake is outstanding. Ordinary channel
    /// hints coalesce and do not start another drain until the delayed wake
    /// clears this flag immediately before the next claim attempt.
    claim_error_retry_pending: AtomicBool,
    /// One-shot inject for claim DB errors (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    claim_error_once: Mutex<Option<DbError>>,
    /// Test gate: pause after claim / before active registration.
    #[cfg(any(test, feature = "test-utils"))]
    pre_register_gates: Mutex<HashMap<i32, Arc<Notify>>>,
    /// Test gate: hold cleanup of a specific (conversation, attempt).
    #[cfg(any(test, feature = "test-utils"))]
    cleanup_holds: Mutex<HashMap<(i32, i32), Arc<Notify>>>,
    /// Count of claim_next_ready attempts (including errors) for tests.
    #[cfg(any(test, feature = "test-utils"))]
    claim_calls: AtomicU64,
    /// Inject failures into record_attempt_failure commits (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    fail_failure_commits: AtomicBool,
    /// Remaining synthetic finalize DB failures before a real commit (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    finalize_fail_remaining: std::sync::atomic::AtomicU32,
    /// When true, `FailureTransition::Ready` does not auto-notify (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    suppress_ready_notify: AtomicBool,
    /// One-shot pause placed at the start of `cancel_all` (test-only).
    /// Arrival fires immediately before root replace/cancel; release continues.
    #[cfg(any(test, feature = "test-utils"))]
    cancel_all_pause: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    /// Count of deadline sweep pass attempts (including injected failures).
    #[cfg(any(test, feature = "test-utils"))]
    sweep_pass_count: AtomicU64,
    /// First next sweep pass returns an error without promoting (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    sweep_fail_once: AtomicBool,
    /// How many times `notification_loop` entered (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    notification_loop_starts: AtomicU64,
    /// How many times `deadline_sweep_loop` entered (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    sweep_loop_starts: AtomicU64,
}

/// Build the production coordinator (hidden runner + manager driver) for
/// desktop and server startup. Keeps driver/runner constructors crate-private.
pub fn build_production_coordinator(
    db: Arc<AppDatabase>,
    connection_manager: ConnectionManager,
    registry: Arc<InternalAgentSessionRegistry>,
    data_dir: PathBuf,
    emitter: EventEmitter,
) -> Arc<AutoTitleCoordinator> {
    let driver: Arc<dyn crate::auto_title::runner::TitleConnectionDriver> = Arc::new(
        ManagerTitleConnectionDriver::new(Arc::new(connection_manager.clone_ref())),
    );
    let runner: Arc<dyn TitleAgentRunner> = Arc::new(HiddenAgentRunner::new(
        Arc::clone(&db),
        driver,
        registry,
        data_dir,
    ));
    let partial: Arc<dyn PartialAssistantTextSource> =
        Arc::new(ManagerPartialSource::from_manager_ref(&connection_manager));
    AutoTitleCoordinator::new_with_deadline(
        db,
        runner,
        emitter,
        partial,
        DEFAULT_DEADLINE,
        DEFAULT_SWEEP_INTERVAL,
        DEFAULT_BATCH_LIMIT,
    )
}

impl AutoTitleCoordinator {
    pub fn new(
        db: Arc<AppDatabase>,
        runner: Arc<dyn TitleAgentRunner>,
        emitter: EventEmitter,
    ) -> Arc<Self> {
        Self::new_with_deadline(
            db,
            runner,
            emitter,
            Arc::new(EmptyPartialSource),
            DEFAULT_DEADLINE,
            DEFAULT_SWEEP_INTERVAL,
            DEFAULT_BATCH_LIMIT,
        )
    }

    /// Construct with injectable partial source and deadline sweep timings.
    pub fn new_with_deadline(
        db: Arc<AppDatabase>,
        runner: Arc<dyn TitleAgentRunner>,
        emitter: EventEmitter,
        partial_source: Arc<dyn PartialAssistantTextSource>,
        deadline: Duration,
        sweep_interval: Duration,
        batch_limit: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            runner,
            emitter,
            partial_source,
            deadline,
            sweep_interval,
            batch_limit,
            attempts: Arc::new(Semaphore::new(MAX_CONCURRENT_ATTEMPTS)),
            notify: Arc::new(Notify::new()),
            active: Mutex::new(HashMap::new()),
            off_root: Mutex::new(CancellationToken::new()),
            started: AtomicBool::new(false),
            claim_error_retry_pending: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-utils"))]
            claim_error_once: Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            pre_register_gates: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "test-utils"))]
            cleanup_holds: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "test-utils"))]
            claim_calls: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-utils"))]
            fail_failure_commits: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-utils"))]
            finalize_fail_remaining: std::sync::atomic::AtomicU32::new(0),
            #[cfg(any(test, feature = "test-utils"))]
            suppress_ready_notify: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-utils"))]
            cancel_all_pause: Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            sweep_pass_count: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-utils"))]
            sweep_fail_once: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-utils"))]
            notification_loop_starts: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-utils"))]
            sweep_loop_starts: AtomicU64::new(0),
        })
    }

    /// Arm a one-shot pause at the start of the next `cancel_all` call.
    ///
    /// Returns `(arrival, release)` where `arrival` resolves immediately before
    /// the off-root is replaced/cancelled, and `release.send(())` lets the
    /// paused `cancel_all` continue. Compiled out of production builds.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn pause_next_cancel_all_before_effect(
        self: &Arc<Self>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (arrival_tx, arrival_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.cancel_all_pause.lock().await = Some((arrival_tx, release_rx));
        (arrival_rx, release_tx)
    }

    /// Inert coordinator for tests that must never invoke a title model.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_inert_for_test(conn: sea_orm::DatabaseConnection) -> Arc<Self> {
        let db = Arc::new(AppDatabase { conn });
        Self::new(db, Arc::new(InertTitleAgentRunner), EventEmitter::Noop)
    }

    pub fn notify_ready(&self) {
        self.notify.notify_one();
    }

    pub async fn recover_and_start(self: &Arc<Self>) -> Result<(), DbError> {
        recover_interrupted_jobs(&self.db.conn).await?;
        register_live_coordinator(self);
        if self
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                this.notification_loop().await;
            });
            let this = Arc::clone(self);
            tokio::spawn(async move {
                this.deadline_sweep_loop().await;
            });
        }
        self.notify_ready();
        Ok(())
    }

    pub async fn cancel_conversation(&self, conversation_id: i32) {
        let mut guard = self.active.lock().await;
        if let Some(active) = guard.remove(&conversation_id) {
            active.cancellation.cancel();
        }
    }

    pub async fn cancel_all(&self) {
        #[cfg(any(test, feature = "test-utils"))]
        {
            if let Some((arrival_tx, release_rx)) = self.cancel_all_pause.lock().await.take() {
                let _ = arrival_tx.send(());
                let _ = release_rx.await;
            }
        }
        let new_root = CancellationToken::new();
        {
            let mut root = self.off_root.lock().await;
            root.cancel();
            *root = new_root;
        }
        let mut guard = self.active.lock().await;
        for (_, active) in guard.drain() {
            active.cancellation.cancel();
        }
    }

    async fn current_off_root(&self) -> CancellationToken {
        self.off_root.lock().await.clone()
    }

    async fn register_active(
        &self,
        conversation_id: i32,
        attempt: i32,
        off_token: CancellationToken,
    ) -> CancellationToken {
        #[cfg(any(test, feature = "test-utils"))]
        {
            let gate = {
                let mut gates = self.pre_register_gates.lock().await;
                gates.remove(&conversation_id)
            };
            if let Some(gate) = gate {
                gate.notified().await;
            }
        }

        let cancellation = off_token.child_token();
        let mut guard = self.active.lock().await;
        guard.insert(
            conversation_id,
            ActiveTitleAttempt {
                attempt,
                cancellation: cancellation.clone(),
            },
        );
        cancellation
    }

    async fn unregister_active(&self, conversation_id: i32, attempt: i32) {
        #[cfg(any(test, feature = "test-utils"))]
        {
            let hold = {
                let holds = self.cleanup_holds.lock().await;
                holds.get(&(conversation_id, attempt)).cloned()
            };
            if let Some(hold) = hold {
                hold.notified().await;
            }
        }

        let mut guard = self.active.lock().await;
        if let Some(active) = guard.get(&conversation_id) {
            if active.attempt == attempt {
                guard.remove(&conversation_id);
            }
        }
    }

    async fn notification_loop(self: Arc<Self>) {
        #[cfg(any(test, feature = "test-utils"))]
        self.notification_loop_starts.fetch_add(1, Ordering::SeqCst);

        let mut claim_error_backoff = ClaimErrorBackoff::default();
        loop {
            self.notify.notified().await;
            // While a claim-error delayed wake is pending, ordinary channel
            // hints coalesce and must not start another drain (or claim).
            if self.claim_error_retry_pending.load(Ordering::SeqCst) {
                continue;
            }
            self.drain_ready(&mut claim_error_backoff).await;
        }
    }

    /// Periodic deadline promotion: immediate first pass, then sleep interval.
    ///
    /// Errors are logged and do not kill the loop. When any job is promoted or
    /// any Ready row remains, `notify_ready` heals lost wake signals.
    async fn deadline_sweep_loop(self: Arc<Self>) {
        #[cfg(any(test, feature = "test-utils"))]
        self.sweep_loop_starts.fetch_add(1, Ordering::SeqCst);

        loop {
            if let Err(error) = self.run_deadline_sweep_once().await {
                tracing::warn!(%error, "auto-title deadline sweep failed");
            }
            tokio::time::sleep(self.sweep_interval).await;
        }
    }

    async fn run_deadline_sweep_once(&self) -> Result<(), DbError> {
        #[cfg(any(test, feature = "test-utils"))]
        {
            self.sweep_pass_count.fetch_add(1, Ordering::SeqCst);
            if self.sweep_fail_once.swap(false, Ordering::SeqCst) {
                return Err(DbError::Validation(
                    "injected deadline sweep failure".into(),
                ));
            }
        }

        let params = DeadlinePromoteParams {
            now: Utc::now(),
            deadline: self.deadline,
            batch_limit: self.batch_limit,
        };
        let ids = list_deadline_candidates(&self.db.conn, &params).await?;
        let partials = if ids.is_empty() {
            HashMap::new()
        } else {
            self.partial_source.partials_for(&ids).await
        };
        let promoted =
            promote_deadline_jobs_by_ids(&self.db.conn, &params, &ids, &partials).await?;
        let has_ready = auto_title_job::Entity::find()
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .limit(1)
            .one(&self.db.conn)
            .await?
            .is_some();
        if promoted > 0 || has_ready {
            self.notify_ready();
        }
        Ok(())
    }

    /// Schedule at most one outstanding delayed wake after a claim DB error.
    fn schedule_unique_delayed_wake(self: &Arc<Self>, delay: Duration) {
        if self
            .claim_error_retry_pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // Clear before the next claim attempt so the delayed wake's
            // notify_ready is allowed to drain.
            this.claim_error_retry_pending
                .store(false, Ordering::SeqCst);
            this.notify_ready();
        });
    }

    async fn drain_ready(self: &Arc<Self>, claim_error_backoff: &mut ClaimErrorBackoff) {
        loop {
            let permit = match self.attempts.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let off_token = self.current_off_root().await.child_token();

            #[cfg(any(test, feature = "test-utils"))]
            self.claim_calls.fetch_add(1, Ordering::SeqCst);

            let claim_result = {
                #[cfg(any(test, feature = "test-utils"))]
                {
                    let mut once = self.claim_error_once.lock().await;
                    if let Some(err) = once.take() {
                        Err(err)
                    } else {
                        claim_next_ready(&self.db.conn).await
                    }
                }
                #[cfg(not(any(test, feature = "test-utils")))]
                {
                    claim_next_ready(&self.db.conn).await
                }
            };

            let claim = match claim_result {
                Ok(Some(claim)) => {
                    claim_error_backoff.reset();
                    claim
                }
                Ok(None) => {
                    claim_error_backoff.reset();
                    drop(permit);
                    break;
                }
                Err(error) => {
                    tracing::warn!(%error, "ready title claim failed");
                    drop(permit);
                    let delay = claim_error_backoff.next_delay();
                    self.schedule_unique_delayed_wake(delay);
                    break;
                }
            };

            let cancellation = self
                .register_active(claim.conversation_id, claim.attempt, off_token)
                .await;

            let still_running = match claim_is_still_running(&self.db.conn, &claim).await {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        conversation_id = claim.conversation_id,
                        %error,
                        "title claim recheck failed"
                    );
                    let this = Arc::clone(self);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let transition = this.settle_attempt_failure_with_retry(&claim).await;
                        this.unregister_active(claim.conversation_id, claim.attempt)
                            .await;
                        if transition == FailureTransition::Ready {
                            #[cfg(any(test, feature = "test-utils"))]
                            let suppress = this.suppress_ready_notify.load(Ordering::SeqCst);
                            #[cfg(not(any(test, feature = "test-utils")))]
                            let suppress = false;
                            if !suppress {
                                this.notify_ready();
                            }
                        }
                    });
                    continue;
                }
            };

            if cancellation.is_cancelled() || !still_running {
                self.unregister_active(claim.conversation_id, claim.attempt)
                    .await;
                drop(permit);
                continue;
            }

            let this = Arc::clone(self);
            tokio::spawn(async move {
                let _permit = permit;
                this.run_claim(claim, cancellation).await;
            });
        }
    }

    async fn run_claim(self: Arc<Self>, claim: AutoTitleClaim, cancellation: CancellationToken) {
        let attempt = AutoTitleAttempt {
            conversation_id: claim.conversation_id,
            attempt: claim.attempt,
            agent: claim.agent,
            locale: claim.locale,
            first_user_text: claim.first_user_text.clone(),
            first_assistant_text: claim.first_assistant_text.clone(),
        };

        let run_result = self.runner.run(attempt, cancellation.child_token()).await;

        match run_result {
            Ok(title) => {
                let outcome = self.settle_finalize_with_retry(&claim, &title).await;
                if matches!(outcome, FinalizeTitleOutcome::Committed) {
                    crate::commands::conversations::emit_conversation_upsert(
                        &self.emitter,
                        &self.db.conn,
                        claim.conversation_id,
                    )
                    .await;
                }
            }
            Err(AutoTitleRunError::Cancelled) => {
                // Cancel paths delete the job via rename/delete/off; nothing else.
            }
            Err(_) => {
                let transition = self.settle_attempt_failure_with_retry(&claim).await;
                if transition == FailureTransition::Ready {
                    #[cfg(any(test, feature = "test-utils"))]
                    let suppress = self.suppress_ready_notify.load(Ordering::SeqCst);
                    #[cfg(not(any(test, feature = "test-utils")))]
                    let suppress = false;
                    if !suppress {
                        self.notify_ready();
                    }
                }
            }
        }

        self.unregister_active(claim.conversation_id, claim.attempt)
            .await;
    }

    pub async fn settle_attempt_failure_with_retry(
        &self,
        claim: &AutoTitleClaim,
    ) -> FailureTransition {
        let mut delay = Duration::from_millis(100);
        loop {
            #[cfg(any(test, feature = "test-utils"))]
            if self.fail_failure_commits.load(Ordering::SeqCst) {
                tokio::time::sleep(delay).await;
                delay = next_backoff(delay);
                continue;
            }

            match record_attempt_failure(&self.db.conn, claim).await {
                Ok(transition) => return transition,
                Err(error) => {
                    tracing::warn!(
                        conversation_id = claim.conversation_id,
                        %error,
                        "title failure transition failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay = next_backoff(delay);
                }
            }
        }
    }

    async fn settle_finalize_with_retry(
        &self,
        claim: &AutoTitleClaim,
        title: &str,
    ) -> FinalizeTitleOutcome {
        let mut delay = Duration::from_millis(100);
        loop {
            #[cfg(any(test, feature = "test-utils"))]
            {
                // Synthetic DB failures: retain runner output; never re-invoke model.
                let remaining = self.finalize_fail_remaining.load(Ordering::SeqCst);
                if remaining > 0 {
                    self.finalize_fail_remaining.fetch_sub(1, Ordering::SeqCst);
                    tracing::warn!(
                        conversation_id = claim.conversation_id,
                        "title finalize failed (injected); retrying"
                    );
                    if !claim_is_still_running(&self.db.conn, claim)
                        .await
                        .unwrap_or(false)
                    {
                        return FinalizeTitleOutcome::Cancelled;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_backoff(delay);
                    continue;
                }
            }

            match finalize_generated_title(&self.db.conn, claim, title).await {
                Ok(outcome) => return outcome,
                Err(error) => {
                    tracing::warn!(
                        conversation_id = claim.conversation_id,
                        %error,
                        "title finalize failed; retrying"
                    );
                    if !claim_is_still_running(&self.db.conn, claim)
                        .await
                        .unwrap_or(false)
                    {
                        return FinalizeTitleOutcome::Cancelled;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_backoff(delay);
                }
            }
        }
    }

    // --- test helpers ---

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn inject_claim_error_once(&self, error: DbError) {
        *self.claim_error_once.lock().await = Some(error);
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn claim_call_count(&self) -> u64 {
        self.claim_calls.load(Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn set_pre_register_gate(&self, conversation_id: i32) -> Arc<Notify> {
        let n = Arc::new(Notify::new());
        self.pre_register_gates
            .lock()
            .await
            .insert(conversation_id, Arc::clone(&n));
        n
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn pause_attempt_cleanup(&self, conversation_id: i32, attempt: i32) -> Arc<Notify> {
        let n = Arc::new(Notify::new());
        self.cleanup_holds
            .lock()
            .await
            .insert((conversation_id, attempt), Arc::clone(&n));
        n
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn release_attempt_cleanup(&self, conversation_id: i32, attempt: i32) {
        if let Some(n) = self
            .cleanup_holds
            .lock()
            .await
            .remove(&(conversation_id, attempt))
        {
            n.notify_waiters();
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_fail_failure_commits(&self, fail: bool) {
        self.fail_failure_commits.store(fail, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_finalize_fail_remaining(&self, n: u32) {
        self.finalize_fail_remaining.store(n, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_suppress_ready_notify(&self, suppress: bool) {
        self.suppress_ready_notify.store(suppress, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn has_active_registration(&self, conversation_id: i32) -> bool {
        self.active.lock().await.contains_key(&conversation_id)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn active_attempt(&self, conversation_id: i32) -> Option<i32> {
        self.active
            .lock()
            .await
            .get(&conversation_id)
            .map(|a| a.attempt)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn claim_error_retry_is_pending(&self) -> bool {
        self.claim_error_retry_pending.load(Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn sweep_pass_count(&self) -> u64 {
        self.sweep_pass_count.load(Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_sweep_fail_once(&self) {
        self.sweep_fail_once.store(true, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn notification_loop_starts(&self) -> u64 {
        self.notification_loop_starts.load(Ordering::SeqCst)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn sweep_loop_starts(&self) -> u64 {
        self.sweep_loop_starts.load(Ordering::SeqCst)
    }
}

fn next_backoff(current: Duration) -> Duration {
    if current < Duration::from_millis(500) {
        Duration::from_millis(500)
    } else {
        Duration::from_secs(5)
    }
}

#[derive(Default)]
struct ClaimErrorBackoff {
    step: u8,
}

impl ClaimErrorBackoff {
    fn reset(&mut self) {
        self.step = 0;
    }

    fn next_delay(&mut self) -> Duration {
        let delay = match self.step {
            0 => Duration::from_millis(100),
            1 => Duration::from_millis(500),
            _ => Duration::from_secs(5),
        };
        self.step = self.step.saturating_add(1);
        delay
    }
}

/// Test-only runner that panics if invoked. Used by inert AppState constructors.
#[cfg(any(test, feature = "test-utils"))]
pub struct InertTitleAgentRunner;

#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
impl TitleAgentRunner for InertTitleAgentRunner {
    async fn run(
        &self,
        _attempt: AutoTitleAttempt,
        _cancellation: CancellationToken,
    ) -> Result<String, AutoTitleRunError> {
        panic!("InertTitleAgentRunner must not be invoked");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
    use tokio::sync::Notify as TokioNotify;
    use tokio::time::{timeout, Duration as TokioDuration};

    use crate::auto_title::types::AutoTitleRunError;
    use crate::commands::conversation_experience::set_auto_title_agent_persisted_core;
    use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
    use crate::db::service::conversation_service::{self, create};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use crate::models::system::AppLocale;

    #[derive(Clone)]
    enum ScriptedStep {
        Fail(AutoTitleRunError),
        Ok(String),
        /// Wait for gate (or cancel); on gate → Ok.
        Block(Arc<TokioNotify>),
        /// Wait for gate (or cancel); on gate → Fail.
        FailWhenReleased(Arc<TokioNotify>),
    }

    struct FakeRunner {
        steps: Mutex<Vec<ScriptedStep>>,
        calls: AtomicUsize,
        cancelled_attempts: Mutex<Vec<i32>>,
    }

    impl FakeRunner {
        fn new(steps: Vec<ScriptedStep>) -> Arc<Self> {
            Arc::new(Self {
                steps: Mutex::new(steps),
                calls: AtomicUsize::new(0),
                cancelled_attempts: Mutex::new(Vec::new()),
            })
        }

        fn fail_twice() -> Arc<Self> {
            Self::new(vec![
                ScriptedStep::Fail(AutoTitleRunError::EmptyOutput),
                ScriptedStep::Fail(AutoTitleRunError::EmptyOutput),
            ])
        }

        fn fail_once() -> Arc<Self> {
            Self::new(vec![ScriptedStep::Fail(AutoTitleRunError::EmptyOutput)])
        }

        fn succeed_once(title: impl Into<String>) -> Arc<Self> {
            Self::new(vec![ScriptedStep::Ok(title.into())])
        }

        fn blocked() -> (Arc<Self>, Arc<TokioNotify>) {
            let n = Arc::new(TokioNotify::new());
            (
                Self::new(vec![
                    ScriptedStep::Block(Arc::clone(&n)),
                    ScriptedStep::Block(Arc::clone(&n)),
                    ScriptedStep::Block(Arc::clone(&n)),
                ]),
                n,
            )
        }

        /// Attempt one fails after release; attempt two blocks until cancelled.
        fn first_fails_second_blocks() -> (Arc<Self>, Arc<TokioNotify>, Arc<TokioNotify>) {
            let release_first = Arc::new(TokioNotify::new());
            let second_block = Arc::new(TokioNotify::new());
            (
                Self::new(vec![
                    ScriptedStep::FailWhenReleased(Arc::clone(&release_first)),
                    ScriptedStep::Block(Arc::clone(&second_block)),
                ]),
                release_first,
                second_block,
            )
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        async fn attempt_two_was_cancelled(&self) -> bool {
            self.cancelled_attempts.lock().await.contains(&2)
        }

        async fn attempt_was_cancelled(&self, attempt: i32) -> bool {
            self.cancelled_attempts.lock().await.contains(&attempt)
        }
    }

    #[async_trait::async_trait]
    impl TitleAgentRunner for FakeRunner {
        async fn run(
            &self,
            attempt: AutoTitleAttempt,
            cancellation: CancellationToken,
        ) -> Result<String, AutoTitleRunError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let step = {
                let mut steps = self.steps.lock().await;
                if steps.is_empty() {
                    ScriptedStep::Fail(AutoTitleRunError::EmptyOutput)
                } else {
                    steps.remove(0)
                }
            };
            match step {
                ScriptedStep::Ok(title) => Ok(title),
                ScriptedStep::Fail(err) => Err(err),
                ScriptedStep::Block(gate) => {
                    tokio::select! {
                        _ = gate.notified() => Ok("blocked-released".into()),
                        _ = cancellation.cancelled() => {
                            self.cancelled_attempts.lock().await.push(attempt.attempt);
                            Err(AutoTitleRunError::Cancelled)
                        }
                    }
                }
                ScriptedStep::FailWhenReleased(gate) => {
                    tokio::select! {
                        _ = gate.notified() => Err(AutoTitleRunError::EmptyOutput),
                        _ = cancellation.cancelled() => {
                            self.cancelled_attempts.lock().await.push(attempt.attempt);
                            Err(AutoTitleRunError::Cancelled)
                        }
                    }
                }
            }
        }
    }

    struct CoordinatorFixture {
        db: AppDatabase,
        folder_id: i32,
        runner: Arc<FakeRunner>,
        coordinator: Arc<AutoTitleCoordinator>,
    }

    async fn coordinator_fixture(runner: Arc<FakeRunner>) -> CoordinatorFixture {
        let db = fresh_in_memory_db().await;
        set_auto_title_agent_persisted_core(&db, Some(AgentType::Codex))
            .await
            .expect("enable titles");
        let folder_id = seed_folder(&db, "/tmp/auto-title-coord").await;
        let title_db = Arc::new(AppDatabase {
            conn: db.conn.clone(),
        });
        let coordinator = AutoTitleCoordinator::new(
            title_db,
            runner.clone() as Arc<dyn TitleAgentRunner>,
            EventEmitter::Noop,
        );
        coordinator.recover_and_start().await.expect("start");
        CoordinatorFixture {
            db,
            folder_id,
            runner,
            coordinator,
        }
    }

    async fn recovery_fixture() -> CoordinatorFixture {
        let db = fresh_in_memory_db().await;
        set_auto_title_agent_persisted_core(&db, Some(AgentType::Codex))
            .await
            .expect("enable titles");
        let folder_id = seed_folder(&db, "/tmp/auto-title-recover").await;
        let title_db = Arc::new(AppDatabase {
            conn: db.conn.clone(),
        });
        let runner = FakeRunner::fail_once();
        let coordinator = AutoTitleCoordinator::new(
            title_db,
            runner.clone() as Arc<dyn TitleAgentRunner>,
            EventEmitter::Noop,
        );
        // Do not start the worker.
        CoordinatorFixture {
            db,
            folder_id,
            runner,
            coordinator,
        }
    }

    async fn seed_conversation(db: &AppDatabase, folder_id: i32) -> i32 {
        create(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("create")
            .id
    }

    async fn seed_job(
        db: &AppDatabase,
        conversation_id: i32,
        state: AutoTitleJobState,
        attempts: i32,
        attempt_turn_seq: i32,
        usable_turn_seq: i32,
    ) {
        // create() may already enroll an awaiting_turn row when titles are On.
        let _ = auto_title_job::Entity::delete_by_id(conversation_id)
            .exec(&db.conn)
            .await;
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(state),
            attempts: Set(attempts),
            first_user_text: Set(Some("user task".into())),
            first_assistant_text: Set(Some("assistant reply".into())),
            first_prompt_at: Set(None),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(usable_turn_seq),
            attempt_turn_seq: Set(attempt_turn_seq),
            last_usable_turn_token: Set(Some(format!("tok-{usable_turn_seq}"))),
            updated_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .expect("seed job");
    }

    impl CoordinatorFixture {
        async fn make_ready(&self, conversation_id: i32, usable_turn_seq: i32) {
            let _ = auto_title_job::Entity::delete_by_id(conversation_id)
                .exec(&self.db.conn)
                .await;
            seed_job(
                &self.db,
                conversation_id,
                AutoTitleJobState::Ready,
                0,
                0,
                usable_turn_seq,
            )
            .await;
        }

        async fn make_three_ready_jobs(&self) {
            for _ in 0..3 {
                let cid = seed_conversation(&self.db, self.folder_id).await;
                seed_job(&self.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
            }
        }

        async fn state(&self, conversation_id: i32) -> Option<AutoTitleJobState> {
            auto_title_job::Entity::find_by_id(conversation_id)
                .one(&self.db.conn)
                .await
                .expect("job")
                .map(|j| j.state)
        }

        async fn attempts(&self, conversation_id: i32) -> i32 {
            auto_title_job::Entity::find_by_id(conversation_id)
                .one(&self.db.conn)
                .await
                .expect("job")
                .map(|j| j.attempts)
                .unwrap_or(0)
        }

        async fn wait_for_state(&self, conversation_id: i32, expected: AutoTitleJobState) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    if self.state(conversation_id).await.as_ref() == Some(&expected) {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("state timeout");
        }

        async fn wait_for_job_deleted(&self, conversation_id: i32) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    if self.state(conversation_id).await.is_none() {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("delete timeout");
        }

        async fn wait_for_running_count(&self, n: usize) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    let count = auto_title_job::Entity::find()
                        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
                        .all(&self.db.conn)
                        .await
                        .expect("list")
                        .len();
                    if count >= n {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("running count timeout");
        }

        async fn ready_count(&self) -> usize {
            auto_title_job::Entity::find()
                .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
                .all(&self.db.conn)
                .await
                .expect("ready")
                .len()
        }

        async fn unclaimed_ready_attempts(&self) -> Vec<i32> {
            auto_title_job::Entity::find()
                .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
                .all(&self.db.conn)
                .await
                .expect("ready")
                .into_iter()
                .map(|j| j.attempts)
                .collect()
        }

        async fn complete_target_turn(&self, conversation_id: i32, token: &str) {
            use crate::auto_title::service::apply_usable_completion;
            use crate::auto_title::types::TurnCompletionSnapshot;
            let txn = self.db.conn.begin().await.expect("txn");
            // Move retry_wait → ready via usable completion.
            let snap = TurnCompletionSnapshot {
                conversation_id,
                turn_token: token.into(),
                locale: AppLocale::En,
                final_text: Arc::from("more assistant text"),
            };
            apply_usable_completion(&txn, &snap, "end_turn")
                .await
                .expect("complete");
            txn.commit().await.expect("commit");
            self.coordinator.notify_ready();
        }

        async fn wait_for_runner_calls(&self, n: usize) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    if self.runner.call_count() >= n {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("runner calls timeout");
        }

        async fn wait_for_active_attempt(&self, conversation_id: i32, attempt: i32) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    if self.coordinator.active_attempt(conversation_id).await == Some(attempt) {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("active attempt timeout");
        }

        async fn wait_for_no_active_registration(&self, conversation_id: i32) {
            timeout(TokioDuration::from_secs(2), async {
                loop {
                    if !self.coordinator.has_active_registration(conversation_id).await
                    {
                        return;
                    }
                    tokio::time::sleep(TokioDuration::from_millis(20)).await;
                }
            })
            .await
            .expect("active registration clear timeout");
        }

        async fn manual_rename(&self, conversation_id: i32, title: &str) {
            let removed =
                conversation_service::update_title(&self.db.conn, conversation_id, title.into())
                    .await
                    .expect("rename");
            if removed {
                self.coordinator.cancel_conversation(conversation_id).await;
            }
        }

        async fn conversation_title(&self, conversation_id: i32) -> Option<String> {
            use crate::db::entities::conversation;
            conversation::Entity::find_by_id(conversation_id)
                .one(&self.db.conn)
                .await
                .expect("conv")
                .and_then(|c| c.title)
        }
    }

    #[tokio::test]
    async fn first_failure_waits_for_next_turn_and_second_failure_deletes_job() {
        let fixture = coordinator_fixture(FakeRunner::fail_twice()).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        fixture.make_ready(cid, 1).await;
        fixture.coordinator.notify_ready();
        fixture
            .wait_for_state(cid, AutoTitleJobState::RetryWait)
            .await;
        assert_eq!(fixture.attempts(cid).await, 1);

        fixture.complete_target_turn(cid, "turn-2").await;
        fixture.wait_for_job_deleted(cid).await;
        assert_eq!(fixture.runner.call_count(), 2);
    }

    #[tokio::test]
    async fn ready_jobs_wait_for_capacity_before_claiming() {
        let (runner, _release) = FakeRunner::blocked();
        let fixture = coordinator_fixture(runner).await;
        fixture.make_three_ready_jobs().await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_running_count(2).await;
        assert_eq!(fixture.ready_count().await, 1);
        assert_eq!(fixture.unclaimed_ready_attempts().await, vec![0]);
    }

    #[tokio::test]
    async fn interrupted_attempt_recovery_counts_started_work() {
        let fixture = recovery_fixture().await;
        let c1 = seed_conversation(&fixture.db, fixture.folder_id).await;
        let c2 = seed_conversation(&fixture.db, fixture.folder_id).await;
        let c3 = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, c1, AutoTitleJobState::Running, 1, 1, 2).await;
        seed_job(&fixture.db, c2, AutoTitleJobState::Running, 1, 1, 1).await;
        seed_job(&fixture.db, c3, AutoTitleJobState::Running, 2, 2, 2).await;

        recover_interrupted_jobs(&fixture.db.conn)
            .await
            .expect("recover");
        assert_eq!(fixture.state(c1).await, Some(AutoTitleJobState::Ready));
        assert_eq!(fixture.state(c2).await, Some(AutoTitleJobState::RetryWait));
        assert_eq!(fixture.state(c3).await, None);
    }

    #[tokio::test]
    async fn failure_transition_db_retry_does_not_rerun_the_model_or_leak_active_state() {
        let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.set_fail_failure_commits(true);
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        // Still holding failure commits — give a beat then allow.
        tokio::time::sleep(TokioDuration::from_millis(150)).await;
        assert_eq!(fixture.runner.call_count(), 1);
        fixture.coordinator.set_fail_failure_commits(false);
        fixture
            .wait_for_state(cid, AutoTitleJobState::RetryWait)
            .await;
        assert_eq!(fixture.runner.call_count(), 1);
        assert!(!fixture.coordinator.has_active_registration(cid).await);
    }

    #[tokio::test]
    async fn orphan_ready_jobs_are_removed_when_setting_is_off() {
        let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
        set_auto_title_agent_persisted_core(&fixture.db, None)
            .await
            .expect("off");
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_job_deleted(cid).await;
        assert_eq!(fixture.runner.call_count(), 0);
    }

    #[tokio::test]
    async fn attempt_one_cleanup_cannot_unregister_attempt_two() {
        let (runner, release_first, _second_block) = FakeRunner::first_fails_second_blocks();
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        fixture.make_ready(cid, 1).await;
        fixture.coordinator.pause_attempt_cleanup(cid, 1).await;
        // Hold Ready long enough to observe it; attempt two starts via explicit notify.
        fixture.coordinator.set_suppress_ready_notify(true);
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        // Bump usable_turn_seq while attempt one still holds the running claim.
        fixture.complete_target_turn(cid, "turn-2").await;
        release_first.notify_waiters();
        fixture.wait_for_state(cid, AutoTitleJobState::Ready).await;
        fixture.coordinator.set_suppress_ready_notify(false);
        // Unrelated queue wake while attempt-one cleanup is held can claim attempt two.
        fixture.coordinator.notify_ready();
        fixture.wait_for_active_attempt(cid, 2).await;
        fixture.coordinator.release_attempt_cleanup(cid, 1).await;
        fixture.manual_rename(cid, "Manual").await;
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.runner.attempt_two_was_cancelled().await {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(20)).await;
            }
        })
        .await
        .expect("attempt two cancelled");
        assert!(fixture.runner.attempt_two_was_cancelled().await);
    }

    #[tokio::test]
    async fn rename_while_runner_is_blocked_cancels_and_late_output_loses() {
        let (runner, _release) = FakeRunner::blocked();
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        fixture.manual_rename(cid, "Manual").await;
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.runner.attempt_was_cancelled(1).await {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(20)).await;
            }
        })
        .await
        .expect("rename cancels blocked runner");
        assert!(fixture.state(cid).await.is_none());
    }

    #[tokio::test]
    async fn new_usable_turn_while_attempt_one_runs_makes_failure_immediately_ready() {
        let release = Arc::new(TokioNotify::new());
        let runner = FakeRunner::new(vec![ScriptedStep::FailWhenReleased(Arc::clone(&release))]);
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        fixture.make_ready(cid, 1).await;
        // Prevent immediate attempt-two claim so Ready is observable.
        fixture.coordinator.set_suppress_ready_notify(true);
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        // Newer usable turn while attempt one is still running.
        fixture.complete_target_turn(cid, "turn-during-run").await;
        release.notify_waiters();
        fixture.wait_for_state(cid, AutoTitleJobState::Ready).await;
        assert_eq!(fixture.attempts(cid).await, 1);
        assert_eq!(fixture.runner.call_count(), 1);
    }

    #[tokio::test]
    async fn database_commit_retry_reuses_one_runner_output() {
        let fixture = coordinator_fixture(FakeRunner::succeed_once("Generated Title")).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.set_finalize_fail_remaining(2);
        fixture.coordinator.notify_ready();
        fixture.wait_for_job_deleted(cid).await;
        // Finalization unregisters after commit; wait so parallel suites cannot
        // observe the brief post-commit registration window.
        fixture.wait_for_no_active_registration(cid).await;
        assert_eq!(fixture.runner.call_count(), 1);
        assert_eq!(
            fixture.conversation_title(cid).await.as_deref(),
            Some("Generated Title")
        );
    }

    #[tokio::test]
    async fn disable_between_claim_and_registration_cancels_without_running() {
        let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        let gate = fixture.coordinator.set_pre_register_gate(cid).await;
        fixture.coordinator.notify_ready();
        fixture
            .wait_for_state(cid, AutoTitleJobState::Running)
            .await;
        // cancel_all alone must cancel the pre-registration Off-root child without
        // relying on Off deleting durable jobs (still_running recheck path).
        fixture.coordinator.cancel_all().await;
        gate.notify_waiters();
        tokio::time::sleep(TokioDuration::from_millis(200)).await;
        assert_eq!(
            fixture.runner.call_count(),
            0,
            "runner must not run after pre-registration cancel_all"
        );
        assert!(
            !fixture.coordinator.has_active_registration(cid).await,
            "active registration must be cleared"
        );
    }

    #[tokio::test]
    async fn rename_between_claim_and_registration_cancels_without_running() {
        let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        let gate = fixture.coordinator.set_pre_register_gate(cid).await;
        fixture.coordinator.notify_ready();
        fixture
            .wait_for_state(cid, AutoTitleJobState::Running)
            .await;
        // Committed rename deletes the job before active registration.
        crate::commands::conversations::update_conversation_title_core(
            &fixture.db.conn,
            fixture.coordinator.as_ref(),
            cid,
            "Manual".into(),
        )
        .await
        .expect("rename");
        gate.notify_waiters();
        tokio::time::sleep(TokioDuration::from_millis(200)).await;
        assert_eq!(
            fixture.runner.call_count(),
            0,
            "runner must not run after pre-registration rename"
        );
        assert!(
            !fixture.coordinator.has_active_registration(cid).await,
            "active registration must be cleared"
        );
        assert!(fixture.state(cid).await.is_none());
    }

    #[tokio::test]
    async fn claim_database_error_does_not_terminate_notification_worker() {
        let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        fixture.make_ready(cid, 1).await;

        // Pause only after real DB I/O for fixture setup (pool timeouts use wall clock).
        tokio::time::pause();

        // recover_and_start already performed one empty drain (notify on start).
        let baseline = fixture.coordinator.claim_call_count();
        fixture
            .coordinator
            .inject_claim_error_once(DbError::Validation("injected claim error".into()))
            .await;
        fixture.coordinator.notify_ready();

        // Wait until the injected claim error is observed.
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.coordinator.claim_call_count() > baseline
                    && fixture.coordinator.claim_error_retry_is_pending()
                {
                    return;
                }
                tokio::time::advance(TokioDuration::from_millis(5)).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("claim error observed");
        assert_eq!(fixture.runner.call_count(), 0);
        let after_error = fixture.coordinator.claim_call_count();

        // Ordinary wake hints during backoff must not multiply claim attempts.
        for _ in 0..5 {
            fixture.coordinator.notify_ready();
            tokio::time::advance(TokioDuration::from_millis(5)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.coordinator.claim_call_count(), after_error);
        assert_eq!(fixture.runner.call_count(), 0);
        assert!(
            fixture.coordinator.claim_error_retry_is_pending(),
            "unique delayed wake must still be outstanding"
        );

        // Drive the 100ms delayed wake under virtual time, then resume so the
        // subsequent claim/run uses normal wall-clock I/O.
        tokio::time::advance(TokioDuration::from_millis(100)).await;
        // Poll the delayed-wake task to clear pending + notify.
        for _ in 0..32 {
            if !fixture.coordinator.claim_error_retry_is_pending() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !fixture.coordinator.claim_error_retry_is_pending(),
            "delayed wake must clear pending before retry claim"
        );
        tokio::time::resume();

        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.coordinator.claim_call_count() > after_error {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("worker retried claim after delayed wake");
        fixture.wait_for_runner_calls(1).await;
        assert_eq!(fixture.runner.call_count(), 1);
        // No additional external notify_ready after the error path.
    }

    #[tokio::test]
    async fn manual_rename_cancels_active_title() {
        let (runner, _release) = FakeRunner::blocked();
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        crate::commands::conversations::update_conversation_title_core(
            &fixture.db.conn,
            fixture.coordinator.as_ref(),
            cid,
            "Manual Win".into(),
        )
        .await
        .expect("rename core");
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.runner.attempt_was_cancelled(1).await {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(20)).await;
            }
        })
        .await
        .expect("active title cancelled");
        assert!(fixture.state(cid).await.is_none());
        assert_eq!(
            fixture.conversation_title(cid).await.as_deref(),
            Some("Manual Win")
        );
    }

    #[tokio::test]
    async fn disabling_titles_cancels_all_and_late_result_loses() {
        let (runner, release) = FakeRunner::blocked();
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;

        // Post-commit Off side effect: delete durable work, then cancel live claims.
        set_auto_title_agent_persisted_core(&fixture.db, None)
            .await
            .expect("off");
        fixture.coordinator.cancel_all().await;

        tokio::time::sleep(TokioDuration::from_millis(200)).await;
        assert!(
            fixture.runner.attempt_was_cancelled(1).await,
            "cancel_all must cancel active blocked title attempt"
        );
        // Late unblock cannot finalize: job gone, claim cancelled.
        release.notify_waiters();
        tokio::time::sleep(TokioDuration::from_millis(50)).await;
        assert!(fixture.state(cid).await.is_none());
        assert_ne!(
            fixture.conversation_title(cid).await.as_deref(),
            Some("blocked-released")
        );
    }

    #[tokio::test]
    async fn soft_delete_cancels_active_title() {
        let (runner, _release) = FakeRunner::blocked();
        let fixture = coordinator_fixture(runner).await;
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(&fixture.db, cid, AutoTitleJobState::Ready, 0, 0, 1).await;
        fixture.coordinator.notify_ready();
        fixture.wait_for_runner_calls(1).await;
        crate::commands::conversations::delete_conversation_core(
            &fixture.db.conn,
            fixture.coordinator.as_ref(),
            cid,
        )
        .await
        .expect("soft delete");
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.runner.attempt_was_cancelled(1).await {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(20)).await;
            }
        })
        .await
        .expect("soft delete cancels");
        assert!(fixture.state(cid).await.is_none());
    }

    // --- Deadline sweep loop + liveness (Task 8) ---

    /// Returns the same partial text for every requested conversation id.
    struct FixedPartialSource {
        text: String,
    }

    #[async_trait::async_trait]
    impl PartialAssistantTextSource for FixedPartialSource {
        async fn partials_for(&self, conversation_ids: &[i32]) -> HashMap<i32, String> {
            conversation_ids
                .iter()
                .map(|&id| (id, self.text.clone()))
                .collect()
        }
    }

    async fn coordinator_with_sweep(
        runner: Arc<FakeRunner>,
        partial: Arc<dyn PartialAssistantTextSource>,
        deadline: Duration,
        sweep_interval: Duration,
        start: bool,
    ) -> CoordinatorFixture {
        let db = fresh_in_memory_db().await;
        set_auto_title_agent_persisted_core(&db, Some(AgentType::Codex))
            .await
            .expect("enable titles");
        let folder_id = seed_folder(&db, "/tmp/auto-title-sweep").await;
        let title_db = Arc::new(AppDatabase {
            conn: db.conn.clone(),
        });
        let coordinator = AutoTitleCoordinator::new_with_deadline(
            title_db,
            runner.clone() as Arc<dyn TitleAgentRunner>,
            EventEmitter::Noop,
            partial,
            deadline,
            sweep_interval,
            64,
        );
        if start {
            coordinator.recover_and_start().await.expect("start");
        }
        CoordinatorFixture {
            db,
            folder_id,
            runner,
            coordinator,
        }
    }

    async fn seed_awaiting_deadline_job(
        db: &AppDatabase,
        conversation_id: i32,
        first_prompt_at: chrono::DateTime<Utc>,
    ) {
        let _ = auto_title_job::Entity::delete_by_id(conversation_id)
            .exec(&db.conn)
            .await;
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(AutoTitleJobState::AwaitingTurn),
            attempts: Set(0),
            first_user_text: Set(Some("user task".into())),
            first_assistant_text: Set(None),
            first_prompt_at: Set(Some(first_prompt_at)),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(0),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(None),
            updated_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .expect("seed awaiting deadline job");
    }

    async fn wait_for_sweep_passes(coordinator: &AutoTitleCoordinator, min: u64) {
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if coordinator.sweep_pass_count() >= min {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "sweep pass timeout: got {} want >= {min}",
                coordinator.sweep_pass_count()
            )
        });
    }

    /// Immediate first pass: long interval so a quick observation cannot be the
    /// post-sleep second tick. Uses real time (sqlx + start_paused conflict).
    #[tokio::test]
    async fn startup_runs_immediate_sweep_before_interval() {
        let runner = FakeRunner::succeed_once("unused");
        let fixture = coordinator_with_sweep(
            runner,
            Arc::new(EmptyPartialSource),
            Duration::ZERO,
            Duration::from_secs(3600),
            false,
        )
        .await;
        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start");
        wait_for_sweep_passes(fixture.coordinator.as_ref(), 1).await;
        assert!(
            fixture.coordinator.sweep_pass_count() >= 1,
            "immediate sweep pass required without waiting for interval"
        );
        assert_eq!(fixture.coordinator.sweep_loop_starts(), 1);
    }

    /// Short real interval + wall sleep (paused time breaks in-memory SQLite
    /// pool acquire on this runtime).
    #[tokio::test]
    async fn sweep_continues_after_transient_failure() {
        let runner = FakeRunner::succeed_once("unused");
        let sweep_interval = Duration::from_millis(50);
        let fixture = coordinator_with_sweep(
            runner,
            Arc::new(EmptyPartialSource),
            Duration::ZERO,
            sweep_interval,
            false,
        )
        .await;
        fixture.coordinator.arm_sweep_fail_once();
        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start");

        wait_for_sweep_passes(fixture.coordinator.as_ref(), 1).await;
        let after_fail = fixture.coordinator.sweep_pass_count();
        assert_eq!(after_fail, 1, "first pass counts even when injected fail");

        tokio::time::sleep(sweep_interval + Duration::from_millis(50)).await;
        wait_for_sweep_passes(fixture.coordinator.as_ref(), after_fail + 1).await;
        assert!(
            fixture.coordinator.sweep_pass_count() > after_fail,
            "loop continues after transient failure"
        );
    }

    #[tokio::test]
    async fn lost_wake_ready_row_is_renotified_and_claimed() {
        let runner = FakeRunner::succeed_once("Lost Wake Title");
        let sweep_interval = Duration::from_millis(50);
        let fixture = coordinator_with_sweep(
            runner,
            Arc::new(EmptyPartialSource),
            Duration::ZERO,
            sweep_interval,
            false,
        )
        .await;
        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start");
        wait_for_sweep_passes(fixture.coordinator.as_ref(), 1).await;

        // Insert Ready after startup notify/drain without an explicit wake.
        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_job(
            &fixture.db,
            cid,
            AutoTitleJobState::Ready,
            0,
            0,
            1,
        )
        .await;

        let claims_before = fixture.coordinator.claim_call_count();
        // Next sweep pass must re-notify; wait past one interval.
        tokio::time::sleep(sweep_interval + Duration::from_millis(50)).await;
        wait_for_sweep_passes(fixture.coordinator.as_ref(), 2).await;

        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.coordinator.claim_call_count() > claims_before {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lost-wake re-notify should drive claim");

        fixture.wait_for_job_deleted(cid).await;
        assert!(fixture.runner.call_count() >= 1);
    }

    #[tokio::test]
    async fn double_recover_and_start_single_notification_and_sweep_loops() {
        let runner = FakeRunner::succeed_once("unused");
        let fixture = coordinator_with_sweep(
            runner,
            Arc::new(EmptyPartialSource),
            Duration::ZERO,
            Duration::from_secs(60),
            false,
        )
        .await;
        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start once");
        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start twice");

        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.coordinator.notification_loop_starts() >= 1
                    && fixture.coordinator.sweep_loop_starts() >= 1
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loops start");

        // Give a second spawn chance to race if CAS were broken.
        tokio::task::yield_now().await;
        tokio::time::sleep(TokioDuration::from_millis(50)).await;

        assert_eq!(fixture.coordinator.notification_loop_starts(), 1);
        assert_eq!(fixture.coordinator.sweep_loop_starts(), 1);
    }

    #[tokio::test]
    async fn sweep_promotes_and_notifies_ready_drain() {
        let runner = FakeRunner::succeed_once("Sweep Promoted Title");
        let sweep_interval = Duration::from_secs(60);
        let partial: Arc<dyn PartialAssistantTextSource> = Arc::new(FixedPartialSource {
            text: "partial assistant from live".into(),
        });
        let fixture = coordinator_with_sweep(
            runner,
            partial,
            Duration::ZERO,
            sweep_interval,
            false,
        )
        .await;

        let cid = seed_conversation(&fixture.db, fixture.folder_id).await;
        seed_awaiting_deadline_job(
            &fixture.db,
            cid,
            Utc::now() - chrono::Duration::seconds(1),
        )
        .await;

        fixture
            .coordinator
            .recover_and_start()
            .await
            .expect("start");

        wait_for_sweep_passes(fixture.coordinator.as_ref(), 1).await;

        // Ready is transient — claim may already be running/finalized.
        timeout(TokioDuration::from_secs(2), async {
            loop {
                if fixture.state(cid).await != Some(AutoTitleJobState::AwaitingTurn) {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(20)).await;
            }
        })
        .await
        .expect("promote off awaiting_turn");

        fixture.wait_for_job_deleted(cid).await;
        assert_eq!(fixture.runner.call_count(), 1);
        assert_eq!(
            fixture.conversation_title(cid).await.as_deref(),
            Some("Sweep Promoted Title")
        );
    }
}
