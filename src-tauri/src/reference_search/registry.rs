//! Guarded pull-job registry for incremental reference search.
//!
//! Owns sequence high-water marks, pre-cancel tombstones, registered-job caps,
//! scan concurrency, page replay, idle/deadline cleanup, and limit epochs.
//! Handlers only await shared page results; registry-owned tasks perform work.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::commands::conversation_experience::{
    MAX_REFERENCE_SEARCH_LIMIT, MIN_REFERENCE_SEARCH_LIMIT,
};
use sea_orm::DatabaseConnection;

use crate::reference_search::matcher::SearchPattern;
use crate::reference_search::sources::{
    CommitCursor, ConversationCursor, FileCursor, ReferenceSourceCursor, ReferenceSourceFactory,
    SourcePage,
};
use crate::reference_search::types::{
    parse_canonical_uuid_v4, validate_source_scope, CancelReferenceSearchRequest,
    NextReferenceSearchPageRequest, ReferenceSearchPage, ReferenceSearchSource, RequestFingerprint,
    SearchIdentity, StartReferenceSearchRequest,
};

/// Production factory that opens file / conversation / commit source cursors.
pub struct ProductionReferenceSourceFactory {
    pub db: DatabaseConnection,
}

#[async_trait::async_trait]
impl ReferenceSourceFactory for ProductionReferenceSourceFactory {
    async fn open(
        &self,
        request: &StartReferenceSearchRequest,
        pattern: SearchPattern,
        limit: usize,
    ) -> Result<Box<dyn ReferenceSourceCursor>, AppCommandError> {
        match request.source {
            ReferenceSearchSource::File => {
                let workspace = request
                    .workspace_path
                    .as_deref()
                    .ok_or_else(|| error_invalid_request("file search requires workspace_path"))?;
                let cursor = FileCursor::open(&self.db, workspace, pattern, limit).await?;
                Ok(Box::new(cursor))
            }
            ReferenceSearchSource::Conversation => Ok(Box::new(ConversationCursor::open(
                self.db.clone(),
                pattern,
                limit,
            ))),
            ReferenceSearchSource::Commit => {
                let workspace = request.workspace_path.as_deref().ok_or_else(|| {
                    error_invalid_request("commit search requires workspace_path")
                })?;
                let cursor = CommitCursor::open(&self.db, workspace, pattern, limit).await?;
                Ok(Box::new(cursor))
            }
        }
    }
}

/// Fixed page size for every first and subsequent resource page.
pub const REFERENCE_SEARCH_PAGE_SIZE: usize = 5;

const MAX_GUARDS: usize = 256;
const MAX_REGISTERED_JOBS: usize = 64;
const MAX_FILE_JOBS: usize = 24;
const MAX_CONVERSATION_JOBS: usize = 32;
const MAX_COMMIT_JOBS: usize = 8;
const MAX_GLOBAL_SCANS: usize = 12;
const MAX_SOURCE_SCANS: usize = 4;

const PAGE_DEADLINE: Duration = Duration::from_secs(30);
const IDLE_TTL: Duration = Duration::from_secs(30);
const PRE_CANCEL_TTL: Duration = Duration::from_secs(30);
const GUARD_RETENTION: Duration = Duration::from_secs(5 * 60);
const SWEEPER_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GuardKey {
    search_session_id: String,
    source: ReferenceSearchSource,
}

struct GuardRecord {
    source_sequence: u64,
    request_id: Uuid,
    fingerprint: Option<RequestFingerprint>,
    limit_epoch: u64,
    terminal: Option<AppCommandError>,
    pre_cancel_until: Option<Instant>,
    retain_until: Instant,
}

struct JobEntry {
    request: StartReferenceSearchRequest,
    request_id: Uuid,
    fingerprint: RequestFingerprint,
    pattern: SearchPattern,
    limit_epoch: u64,
    result_limit: usize,
    cancel: CancellationToken,
    cursor: Option<Box<dyn ReferenceSourceCursor>>,
    page_zero: Option<ReferenceSearchPage>,
    latest_page: Option<ReferenceSearchPage>,
    /// Next page index that may advance the cursor (0 until page zero publishes).
    next_expected: u32,
    notify: Arc<Notify>,
    in_flight: Option<u32>,
    last_activity: Instant,
}

struct RegistryState {
    guards: HashMap<GuardKey, GuardRecord>,
    jobs: HashMap<GuardKey, JobEntry>,
    limit: u16,
    limit_epoch: u64,
}

/// Process-wide registry for guarded incremental reference search jobs.
pub struct ReferenceSearchRegistry {
    inner: Mutex<RegistryState>,
    factory: Arc<dyn ReferenceSourceFactory>,
    global_scan: Arc<Semaphore>,
    file_scan: Arc<Semaphore>,
    conversation_scan: Arc<Semaphore>,
    commit_scan: Arc<Semaphore>,
    #[cfg(test)]
    advance_counter: Arc<AtomicUsize>,
    /// One-shot pause at the start of the next `set_limit` (before epoch
    /// cancellation). Armed by tests so concurrent limit wrappers can prove
    /// the mutation gate is held through registry application.
    #[cfg(test)]
    limit_apply_pause: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

impl ReferenceSearchRegistry {
    pub fn new(limit: u16, factory: Arc<dyn ReferenceSourceFactory>) -> Arc<Self> {
        #[cfg(test)]
        {
            Self::new_with_counter(limit, factory, Arc::new(AtomicUsize::new(0)))
        }
        #[cfg(not(test))]
        {
            let limit = clamp_reference_search_limit_for_registry(limit);
            Arc::new(Self {
                inner: Mutex::new(RegistryState {
                    guards: HashMap::new(),
                    jobs: HashMap::new(),
                    limit,
                    limit_epoch: 0,
                }),
                factory,
                global_scan: Arc::new(Semaphore::new(MAX_GLOBAL_SCANS)),
                file_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
                conversation_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
                commit_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
            })
        }
    }

    #[cfg(test)]
    fn new_with_counter(
        limit: u16,
        factory: Arc<dyn ReferenceSourceFactory>,
        advance_counter: Arc<AtomicUsize>,
    ) -> Arc<Self> {
        let limit = clamp_reference_search_limit_for_registry(limit);
        Arc::new(Self {
            inner: Mutex::new(RegistryState {
                guards: HashMap::new(),
                jobs: HashMap::new(),
                limit,
                limit_epoch: 0,
            }),
            factory,
            global_scan: Arc::new(Semaphore::new(MAX_GLOBAL_SCANS)),
            file_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
            conversation_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
            commit_scan: Arc::new(Semaphore::new(MAX_SOURCE_SCANS)),
            advance_counter,
            limit_apply_pause: Mutex::new(None),
        })
    }

    /// Arm a one-shot pause at the start of the next `set_limit` call, before
    /// epoch cancellation. Returns `(arrival, release)`. Compiled out of
    /// production.
    #[cfg(test)]
    pub async fn pause_next_limit_apply_before_effect(
        self: &Arc<Self>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (arrival_tx, arrival_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.limit_apply_pause.lock().await = Some((arrival_tx, release_rx));
        (arrival_rx, release_tx)
    }

    #[cfg(test)]
    pub async fn current_limit(&self) -> u16 {
        self.inner.lock().await.limit
    }

    #[cfg(test)]
    pub async fn current_limit_epoch(&self) -> u64 {
        self.inner.lock().await.limit_epoch
    }

    pub async fn start(
        self: &Arc<Self>,
        request: StartReferenceSearchRequest,
    ) -> Result<ReferenceSearchPage, AppCommandError> {
        let key = GuardKey {
            search_session_id: request.search_session_id.clone(),
            source: request.source,
        };
        let fingerprint = RequestFingerprint::from_start(&request);

        {
            let mut state = self.inner.lock().await;
            let now = Instant::now();
            state.sweep_guards(now);

            let identity = SearchIdentity::parse(
                &request.search_session_id,
                request.source_sequence,
                &request.request_id,
            )
            .map_err(AppCommandError::from)?;
            let request_id =
                parse_canonical_uuid_v4(&identity.request_id).map_err(AppCommandError::from)?;

            if let Some(guard) = state.guards.get(&key) {
                if request.source_sequence < guard.source_sequence {
                    return Err(error_stale_start());
                }
                if request.source_sequence == guard.source_sequence {
                    if guard.request_id != request_id {
                        return Err(error_invalid_request(
                            "request_id does not match high-water for this source sequence",
                        ));
                    }
                    if let Some(existing_fp) = &guard.fingerprint {
                        if existing_fp != &fingerprint {
                            return Err(error_invalid_request(
                                "immutable arguments do not match high-water for this source sequence",
                            ));
                        }
                    }

                    if let Some(until) = guard.pre_cancel_until {
                        if now < until {
                            if let Some(g) = state.guards.get_mut(&key) {
                                g.pre_cancel_until = None;
                                g.fingerprint = Some(fingerprint.clone());
                                g.terminal = Some(error_cancelled());
                                g.retain_until = now + GUARD_RETENTION;
                            }
                            return Err(error_cancelled());
                        }
                    }

                    if let Some(terminal) = state.guards.get(&key).and_then(|g| g.terminal.clone())
                    {
                        return Err(terminal);
                    }

                    if state.jobs.contains_key(&key) {
                        if let Some(job) = state.jobs.get_mut(&key) {
                            if job.request_id == request_id
                                && job.fingerprint == fingerprint
                                && job.request.source_sequence == request.source_sequence
                            {
                                job.last_activity = now;
                            } else {
                                return Err(error_invalid_request(
                                    "live job identity does not match start request",
                                ));
                            }
                        }
                        // join below
                    } else {
                        // High water may have advanced without a slot (e.g. brief
                        // overload). Retry registration for the same identity.
                        admit_new_job(
                            &mut state,
                            &key,
                            &request,
                            request_id,
                            fingerprint.clone(),
                            now,
                        )?;
                    }
                } else {
                    // Higher sequence: admit under high water below.
                    admit_higher_start(
                        &mut state,
                        &key,
                        &request,
                        request_id,
                        fingerprint.clone(),
                        now,
                    )?;
                }
            } else {
                admit_higher_start(
                    &mut state,
                    &key,
                    &request,
                    request_id,
                    fingerprint.clone(),
                    now,
                )?;
            }
        }

        self.await_page(key, 0).await
    }

    pub async fn next_page(
        self: &Arc<Self>,
        request: NextReferenceSearchPageRequest,
    ) -> Result<ReferenceSearchPage, AppCommandError> {
        let key = GuardKey {
            search_session_id: request.search_session_id.clone(),
            source: request.source,
        };

        {
            let mut state = self.inner.lock().await;
            let now = Instant::now();
            state.sweep_guards(now);

            let identity = SearchIdentity::parse(
                &request.search_session_id,
                request.source_sequence,
                &request.request_id,
            )
            .map_err(AppCommandError::from)?;
            let request_id =
                parse_canonical_uuid_v4(&identity.request_id).map_err(AppCommandError::from)?;

            if let Some(guard) = state.guards.get(&key) {
                if request.source_sequence < guard.source_sequence {
                    return Err(error_stale_start());
                }
                if request.source_sequence == guard.source_sequence {
                    if guard.request_id != request_id {
                        return Err(error_invalid_request(
                            "request_id does not match high-water for this source sequence",
                        ));
                    }
                    if let Some(terminal) = &guard.terminal {
                        return Err(terminal.clone());
                    }
                }
            }

            let Some(job) = state.jobs.get_mut(&key) else {
                return Err(error_job_expired());
            };
            if job.request.source_sequence != request.source_sequence
                || job.request_id != request_id
            {
                return Err(error_stale_page());
            }

            // Admit replay / advance under lock and refresh activity.
            if request.page_index == 0 {
                if let Some(page) = job.page_zero.clone() {
                    job.last_activity = now;
                    return Ok(page);
                }
            } else if let Some(page) = job.latest_page.clone() {
                if page.page_index == request.page_index {
                    job.last_activity = now;
                    return Ok(page);
                }
            }

            if request.page_index != job.next_expected {
                return Err(error_stale_page());
            }
            if let Some(latest) = &job.latest_page {
                if latest.done {
                    return Err(error_stale_page());
                }
            }

            job.last_activity = now;
        }

        self.await_page(key, request.page_index).await
    }

    pub async fn cancel(
        &self,
        request: CancelReferenceSearchRequest,
    ) -> Result<bool, AppCommandError> {
        let mut state = self.inner.lock().await;
        let now = Instant::now();
        state.sweep_guards(now);

        let identity = SearchIdentity::parse(
            &request.search_session_id,
            request.source_sequence,
            &request.request_id,
        )
        .map_err(AppCommandError::from)?;
        let request_id =
            parse_canonical_uuid_v4(&identity.request_id).map_err(AppCommandError::from)?;
        let key = GuardKey {
            search_session_id: identity.search_session_id,
            source: request.source,
        };

        if let Some(guard) = state.guards.get(&key) {
            if request.source_sequence < guard.source_sequence {
                return Ok(false);
            }
            if request.source_sequence == guard.source_sequence {
                if guard.request_id != request_id {
                    return Ok(false);
                }
                let removed = take_job(&mut state, &key, Some(error_cancelled()), now);
                if let Some(guard) = state.guards.get_mut(&key) {
                    guard.terminal = Some(error_cancelled());
                    guard.pre_cancel_until = None;
                    guard.retain_until = now + GUARD_RETENTION;
                }
                if let Some(cleanup) = removed {
                    finish_cleanup(cleanup);
                    return Ok(true);
                }
                return Ok(false);
            }
        }

        // Higher sequence or missing guard: install pre-cancel tombstone.
        if !state.guards.contains_key(&key) && state.guards.len() >= MAX_GUARDS {
            return Err(error_overloaded());
        }

        let removed = take_job(&mut state, &key, Some(error_cancelled()), now);
        let limit_epoch = state.limit_epoch;
        state.guards.insert(
            key,
            GuardRecord {
                source_sequence: request.source_sequence,
                request_id,
                fingerprint: None,
                limit_epoch,
                terminal: Some(error_cancelled()),
                pre_cancel_until: Some(now + PRE_CANCEL_TTL),
                retain_until: now + GUARD_RETENTION,
            },
        );
        let had_job = removed.is_some();
        if let Some(cleanup) = removed {
            finish_cleanup(cleanup);
        }
        Ok(had_job)
    }

    /// Clamp and publish a new result limit, cancel every old-epoch job, return
    /// the new limit epoch.
    pub async fn set_limit(&self, limit: u16) -> u64 {
        let limit = clamp_reference_search_limit_for_registry(limit);

        #[cfg(test)]
        {
            if let Some((arrival_tx, release_rx)) = self.limit_apply_pause.lock().await.take() {
                let _ = arrival_tx.send(());
                let _ = release_rx.await;
            }
        }

        let mut state = self.inner.lock().await;
        let now = Instant::now();
        state.limit = limit;
        state.limit_epoch = state.limit_epoch.saturating_add(1);
        let epoch = state.limit_epoch;
        let error = error_limit_epoch_changed();

        let keys: Vec<GuardKey> = state.jobs.keys().cloned().collect();
        let mut cleanups = Vec::new();
        for key in keys {
            if let Some(cleanup) = take_job(&mut state, &key, Some(error.clone()), now) {
                if let Some(guard) = state.guards.get_mut(&key) {
                    guard.terminal = Some(error.clone());
                    guard.limit_epoch = epoch;
                    guard.retain_until = now + GUARD_RETENTION;
                    guard.pre_cancel_until = None;
                }
                cleanups.push(cleanup);
            }
        }
        drop(state);
        for cleanup in cleanups {
            finish_cleanup(cleanup);
        }
        epoch
    }

    pub async fn sweep_expired(&self, now: Instant) {
        let mut state = self.inner.lock().await;
        let mut cleanups = Vec::new();

        let expired_jobs: Vec<GuardKey> = state
            .jobs
            .iter()
            .filter(|(_, job)| now.saturating_duration_since(job.last_activity) >= IDLE_TTL)
            .map(|(key, _)| key.clone())
            .collect();

        for key in expired_jobs {
            if let Some(cleanup) = take_job(&mut state, &key, Some(error_job_expired()), now) {
                if let Some(guard) = state.guards.get_mut(&key) {
                    guard.terminal = Some(error_job_expired());
                    guard.retain_until = now + GUARD_RETENTION;
                    guard.pre_cancel_until = None;
                }
                cleanups.push(cleanup);
            }
        }

        state.sweep_guards(now);
        drop(state);
        for cleanup in cleanups {
            finish_cleanup(cleanup);
        }
    }

    async fn await_page(
        self: &Arc<Self>,
        key: GuardKey,
        page_index: u32,
    ) -> Result<ReferenceSearchPage, AppCommandError> {
        loop {
            let notify = {
                let mut state = self.inner.lock().await;
                let now = Instant::now();

                if let Some(guard) = state.guards.get(&key) {
                    if let Some(terminal) = &guard.terminal {
                        if !state.jobs.contains_key(&key) {
                            return Err(terminal.clone());
                        }
                    }
                }

                let Some(job) = state.jobs.get_mut(&key) else {
                    return match state.guards.get(&key).and_then(|g| g.terminal.clone()) {
                        Some(error) => Err(error),
                        None => Err(error_job_expired()),
                    };
                };

                if page_index == 0 {
                    if let Some(page) = job.page_zero.clone() {
                        job.last_activity = now;
                        return Ok(page);
                    }
                } else if let Some(page) = job.latest_page.clone() {
                    if page.page_index == page_index {
                        job.last_activity = now;
                        return Ok(page);
                    }
                }

                if page_index != 0
                    && page_index != job.next_expected
                    && job.latest_page.as_ref().map(|p| p.page_index) != Some(page_index)
                    && job.page_zero.as_ref().map(|p| p.page_index) != Some(page_index)
                {
                    return Err(error_stale_page());
                }

                if job.in_flight != Some(page_index) {
                    if page_index == job.next_expected
                        || (page_index == 0 && job.page_zero.is_none())
                    {
                        job.in_flight = Some(page_index);
                        let registry = Arc::clone(self);
                        let task_key = key.clone();
                        let deadline = Instant::now() + PAGE_DEADLINE;
                        tokio::spawn(async move {
                            registry.execute_page(task_key, page_index, deadline).await;
                        });
                    } else {
                        return Err(error_stale_page());
                    }
                }

                job.notify.clone()
            };

            notify.notified().await;
        }
    }

    async fn execute_page(self: Arc<Self>, key: GuardKey, page_index: u32, deadline: Instant) {
        let (cancel, source, need_open, pattern, request, result_limit, limit_epoch, request_id) = {
            let state = self.inner.lock().await;
            let Some(job) = state.jobs.get(&key) else {
                return;
            };
            if job.in_flight != Some(page_index) {
                return;
            }
            (
                job.cancel.clone(),
                job.request.source,
                job.cursor.is_none(),
                job.pattern.clone(),
                job.request.clone(),
                job.result_limit,
                job.limit_epoch,
                job.request_id,
            )
        };

        let permits = match self.acquire_scan_permits(source, &cancel, deadline).await {
            Ok(permits) => permits,
            Err(error) => {
                self.fail_job(key, error).await;
                return;
            }
        };

        if need_open {
            let open_result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(error_cancelled()),
                _ = tokio::time::sleep_until(deadline) => Err(error_source_timeout()),
                result = self.factory.open(&request, pattern, result_limit) => result,
            };
            match open_result {
                Ok(cursor) => {
                    let mut state = self.inner.lock().await;
                    if !job_still_current(&state, &key, request_id, limit_epoch) {
                        drop(state);
                        drop(permits);
                        let mut cursor = cursor;
                        tokio::spawn(async move {
                            cursor.close().await;
                        });
                        return;
                    }
                    if let Some(job) = state.jobs.get_mut(&key) {
                        job.cursor = Some(cursor);
                    }
                }
                Err(error) => {
                    drop(permits);
                    self.fail_job(key, error).await;
                    return;
                }
            }
        }

        let scan_result = {
            let mut state = self.inner.lock().await;
            if !job_still_current(&state, &key, request_id, limit_epoch) {
                drop(state);
                drop(permits);
                return;
            }
            let Some(job) = state.jobs.get_mut(&key) else {
                drop(state);
                drop(permits);
                return;
            };
            // Cursor next_page must run without holding the registry mutex.
            // Temporarily take the cursor.
            let Some(mut cursor) = job.cursor.take() else {
                drop(state);
                drop(permits);
                self.fail_job(key, error_source_failed("source cursor missing"))
                    .await;
                return;
            };
            drop(state);

            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(error_cancelled()),
                _ = tokio::time::sleep_until(deadline) => Err(error_source_timeout()),
                result = cursor.next_page(REFERENCE_SEARCH_PAGE_SIZE, cancel.child_token()) => result,
            };

            // Restore cursor unless the job disappeared or we fail.
            let mut state = self.inner.lock().await;
            if let Some(job) = state.jobs.get_mut(&key) {
                if job.request_id == request_id && job.limit_epoch == limit_epoch {
                    job.cursor = Some(cursor);
                } else {
                    tokio::spawn(async move {
                        cursor.close().await;
                    });
                }
            } else {
                tokio::spawn(async move {
                    cursor.close().await;
                });
            }
            result
        };

        drop(permits);

        match scan_result {
            Ok(source_page) => {
                self.publish_page(key, page_index, request_id, limit_epoch, source_page)
                    .await;
            }
            Err(error) => {
                self.fail_job(key, error).await;
            }
        }
    }

    async fn publish_page(
        &self,
        key: GuardKey,
        page_index: u32,
        request_id: Uuid,
        limit_epoch: u64,
        source_page: SourcePage,
    ) {
        let mut cursor_to_close = None;
        {
            let mut state = self.inner.lock().await;
            let Some(job) = state.jobs.get_mut(&key) else {
                return;
            };
            if job.request_id != request_id || job.limit_epoch != limit_epoch {
                return;
            }
            if job.in_flight != Some(page_index) {
                return;
            }

            let done = source_page.done;
            let page = ReferenceSearchPage {
                source_sequence: job.request.source_sequence,
                request_id: job.request.request_id.clone(),
                page_index,
                items: source_page.items,
                source_epoch: source_page.source_epoch,
                done,
                done_reason: source_page.done_reason,
            };

            if page_index == 0 {
                job.page_zero = Some(page.clone());
            }
            job.latest_page = Some(page);
            job.next_expected = page_index.saturating_add(1);
            job.in_flight = None;
            if done {
                cursor_to_close = job.cursor.take();
            }
            job.notify.notify_waiters();
        }

        if let Some(mut cursor) = cursor_to_close {
            tokio::spawn(async move {
                cursor.close().await;
            });
        }
    }

    async fn fail_job(&self, key: GuardKey, error: AppCommandError) {
        let now = Instant::now();
        let cleanup = {
            let mut state = self.inner.lock().await;
            // If cancel/limit/sweep already removed the job, keep their terminal
            // error and do not overwrite LimitEpochChanged/Cancelled with a
            // late page-task Cancelled/Timeout.
            if !state.jobs.contains_key(&key) {
                return;
            }
            take_job(&mut state, &key, Some(error), now)
        };
        if let Some(cleanup) = cleanup {
            finish_cleanup(cleanup);
        }
    }

    async fn acquire_scan_permits(
        &self,
        source: ReferenceSearchSource,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), AppCommandError> {
        let source_sem = self.source_semaphore(source);

        let source_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(error_cancelled()),
            _ = tokio::time::sleep_until(deadline) => return Err(error_source_timeout()),
            permit = source_sem.clone().acquire_owned() => {
                permit.map_err(|_| error_source_failed("source scan semaphore closed"))?
            }
        };

        let global_permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                drop(source_permit);
                return Err(error_cancelled());
            }
            _ = tokio::time::sleep_until(deadline) => {
                drop(source_permit);
                return Err(error_source_timeout());
            }
            permit = self.global_scan.clone().acquire_owned() => {
                match permit {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(source_permit);
                        return Err(error_source_failed("global scan semaphore closed"));
                    }
                }
            }
        };

        Ok((source_permit, global_permit))
    }

    fn source_semaphore(&self, source: ReferenceSearchSource) -> Arc<Semaphore> {
        match source {
            ReferenceSearchSource::File => Arc::clone(&self.file_scan),
            ReferenceSearchSource::Conversation => Arc::clone(&self.conversation_scan),
            ReferenceSearchSource::Commit => Arc::clone(&self.commit_scan),
        }
    }

    #[cfg(test)]
    pub async fn registered_count(&self) -> usize {
        self.inner.lock().await.jobs.len()
    }

    #[cfg(test)]
    pub fn cursor_advance_count(&self) -> usize {
        self.advance_counter.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub async fn guard_count(&self) -> usize {
        self.inner.lock().await.guards.len()
    }
}

/// Background sweeper: idle jobs and expired high-water guards.
pub async fn run_reference_search_sweeper(registry: Arc<ReferenceSearchRegistry>) {
    let mut interval = tokio::time::interval(SWEEPER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        registry.sweep_expired(Instant::now()).await;
    }
}

struct JobCleanup {
    cancel: CancellationToken,
    cursor: Option<Box<dyn ReferenceSourceCursor>>,
    notify: Arc<Notify>,
}

fn take_job(
    state: &mut RegistryState,
    key: &GuardKey,
    terminal: Option<AppCommandError>,
    now: Instant,
) -> Option<JobCleanup> {
    let job = state.jobs.remove(key)?;
    if let Some(error) = terminal {
        if let Some(guard) = state.guards.get_mut(key) {
            if guard.source_sequence == job.request.source_sequence
                && guard.request_id == job.request_id
            {
                guard.terminal = Some(error);
                guard.retain_until = now + GUARD_RETENTION;
                guard.pre_cancel_until = None;
            }
        }
    }
    Some(JobCleanup {
        cancel: job.cancel,
        cursor: job.cursor,
        notify: job.notify,
    })
}

fn finish_cleanup(cleanup: JobCleanup) {
    cleanup.cancel.cancel();
    cleanup.notify.notify_waiters();
    if let Some(mut cursor) = cleanup.cursor {
        tokio::spawn(async move {
            cursor.close().await;
        });
    }
}

fn job_still_current(
    state: &RegistryState,
    key: &GuardKey,
    request_id: Uuid,
    limit_epoch: u64,
) -> bool {
    state
        .jobs
        .get(key)
        .is_some_and(|job| job.request_id == request_id && job.limit_epoch == limit_epoch)
}

impl RegistryState {
    fn sweep_guards(&mut self, now: Instant) {
        self.guards.retain(|_, guard| guard.retain_until > now);
    }
}

fn source_job_cap(source: ReferenceSearchSource) -> usize {
    match source {
        ReferenceSearchSource::File => MAX_FILE_JOBS,
        ReferenceSearchSource::Conversation => MAX_CONVERSATION_JOBS,
        ReferenceSearchSource::Commit => MAX_COMMIT_JOBS,
    }
}

fn validate_start_arguments(request: &StartReferenceSearchRequest) -> Result<(), AppCommandError> {
    validate_source_scope(request.source, request.workspace_path.as_deref())
        .map_err(AppCommandError::from)
}

fn admit_higher_start(
    state: &mut RegistryState,
    key: &GuardKey,
    request: &StartReferenceSearchRequest,
    request_id: Uuid,
    fingerprint: RequestFingerprint,
    now: Instant,
) -> Result<(), AppCommandError> {
    if !state.guards.contains_key(key) && state.guards.len() >= MAX_GUARDS {
        return Err(error_overloaded());
    }

    // Replace/cancel any lower live job for this key.
    if let Some(cleanup) = take_job(state, key, Some(error_cancelled()), now) {
        finish_cleanup(cleanup);
    }

    // Snapshot limit/epoch before validation outcomes that skip registration.
    let limit_epoch = state.limit_epoch;

    // Advance high water first (may store validation failures without a slot).
    state.guards.insert(
        key.clone(),
        GuardRecord {
            source_sequence: request.source_sequence,
            request_id,
            fingerprint: Some(fingerprint.clone()),
            limit_epoch,
            terminal: None,
            pre_cancel_until: None,
            retain_until: now + GUARD_RETENTION,
        },
    );

    if let Err(error) = validate_start_arguments(request) {
        if let Some(guard) = state.guards.get_mut(key) {
            guard.terminal = Some(error.clone());
            guard.retain_until = now + GUARD_RETENTION;
        }
        return Err(error);
    }

    let pattern = match SearchPattern::parse(&request.query) {
        Ok(pattern) => pattern,
        Err(error) => {
            let app_error = AppCommandError::from(error);
            if let Some(guard) = state.guards.get_mut(key) {
                guard.terminal = Some(app_error.clone());
                guard.retain_until = now + GUARD_RETENTION;
            }
            return Err(app_error);
        }
    };

    admit_new_job_with_pattern(state, key, request, request_id, fingerprint, pattern, now)
}

fn admit_new_job(
    state: &mut RegistryState,
    key: &GuardKey,
    request: &StartReferenceSearchRequest,
    request_id: Uuid,
    fingerprint: RequestFingerprint,
    now: Instant,
) -> Result<(), AppCommandError> {
    if let Err(error) = validate_start_arguments(request) {
        if let Some(guard) = state.guards.get_mut(key) {
            guard.terminal = Some(error.clone());
            guard.retain_until = now + GUARD_RETENTION;
        }
        return Err(error);
    }
    let pattern = match SearchPattern::parse(&request.query) {
        Ok(pattern) => pattern,
        Err(error) => {
            let app_error = AppCommandError::from(error);
            if let Some(guard) = state.guards.get_mut(key) {
                guard.terminal = Some(app_error.clone());
                guard.retain_until = now + GUARD_RETENTION;
            }
            return Err(app_error);
        }
    };
    admit_new_job_with_pattern(state, key, request, request_id, fingerprint, pattern, now)
}

fn admit_new_job_with_pattern(
    state: &mut RegistryState,
    key: &GuardKey,
    request: &StartReferenceSearchRequest,
    request_id: Uuid,
    fingerprint: RequestFingerprint,
    pattern: SearchPattern,
    now: Instant,
) -> Result<(), AppCommandError> {
    if state.jobs.len() >= MAX_REGISTERED_JOBS {
        return Err(error_overloaded());
    }
    let source_count = state
        .jobs
        .values()
        .filter(|job| job.request.source == request.source)
        .count();
    if source_count >= source_job_cap(request.source) {
        return Err(error_overloaded());
    }

    let limit_epoch = state.limit_epoch;
    let result_limit = state.limit as usize;

    // Keep high-water fingerprint/epoch in sync with the live job.
    if let Some(guard) = state.guards.get_mut(key) {
        guard.fingerprint = Some(fingerprint.clone());
        guard.limit_epoch = limit_epoch;
        guard.terminal = None;
        guard.pre_cancel_until = None;
        guard.retain_until = now + GUARD_RETENTION;
    }

    state.jobs.insert(
        key.clone(),
        JobEntry {
            request: request.clone(),
            request_id,
            fingerprint,
            pattern,
            limit_epoch,
            result_limit,
            cancel: CancellationToken::new(),
            cursor: None,
            page_zero: None,
            latest_page: None,
            next_expected: 0,
            notify: Arc::new(Notify::new()),
            in_flight: None,
            last_activity: now,
        },
    );
    Ok(())
}

fn error_cancelled() -> AppCommandError {
    AppCommandError::new(AppErrorCode::Cancelled, "reference search cancelled")
}

fn error_stale_start() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::StaleStart,
        "reference search start is stale for this source",
    )
}

fn error_stale_page() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::StalePage,
        "reference search page index is stale for this job",
    )
}

fn error_job_expired() -> AppCommandError {
    AppCommandError::new(AppErrorCode::JobExpired, "reference search job expired")
}

fn error_limit_epoch_changed() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::LimitEpochChanged,
        "reference search limit epoch changed",
    )
}

fn error_overloaded() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::RegistryOverloaded,
        "reference search registry is at capacity",
    )
}

fn error_source_timeout() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::SourceTimeout,
        "reference search page deadline exceeded",
    )
}

fn error_source_failed(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::SourceFailed, message)
}

fn error_invalid_request(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::InvalidRequest, message)
}

fn clamp_reference_search_limit_for_registry(limit: u16) -> u16 {
    limit.clamp(MIN_REFERENCE_SEARCH_LIMIT, MAX_REFERENCE_SEARCH_LIMIT)
}

// ─── Test support and unit tests ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio::time::{timeout, Duration};

    use crate::reference_search::types::{
        ReferenceCandidate, ReferenceCandidateMetadata, ReferenceDoneReason, ReferenceFileKind,
    };

    /// Canonical v4 session/request fixture.
    pub const UUID_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    trait RequestTestExt {
        fn cancel_request(&self) -> CancelReferenceSearchRequest;
        fn next_request(&self, page_index: u32) -> NextReferenceSearchPageRequest;
    }

    impl RequestTestExt for StartReferenceSearchRequest {
        fn cancel_request(&self) -> CancelReferenceSearchRequest {
            CancelReferenceSearchRequest {
                search_session_id: self.search_session_id.clone(),
                source_sequence: self.source_sequence,
                request_id: self.request_id.clone(),
                source: self.source,
            }
        }

        fn next_request(&self, page_index: u32) -> NextReferenceSearchPageRequest {
            NextReferenceSearchPageRequest {
                search_session_id: self.search_session_id.clone(),
                source_sequence: self.source_sequence,
                request_id: self.request_id.clone(),
                source: self.source,
                page_index,
            }
        }
    }

    struct CountingFactory {
        candidate_count: usize,
        advances: Arc<AtomicUsize>,
    }

    struct CountingCursor {
        remaining: usize,
        produced: usize,
        limit: usize,
        source: ReferenceSearchSource,
        advances: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ReferenceSourceCursor for CountingCursor {
        async fn next_page(
            &mut self,
            page_size: usize,
            _token: CancellationToken,
        ) -> Result<SourcePage, AppCommandError> {
            self.advances.fetch_add(1, Ordering::SeqCst);
            let budget = self.limit.saturating_sub(self.produced);
            let take = page_size.min(self.remaining).min(budget);
            let mut items = Vec::with_capacity(take);
            for _ in 0..take {
                let ordinal = self.produced as u64;
                items.push(synthetic_candidate(self.source, ordinal));
                self.produced += 1;
                self.remaining = self.remaining.saturating_sub(1);
            }
            let hit_limit = self.produced >= self.limit;
            let exhausted = self.remaining == 0;
            let done = hit_limit || exhausted;
            let done_reason = if done {
                if hit_limit {
                    Some(ReferenceDoneReason::Limit)
                } else {
                    Some(ReferenceDoneReason::Exhausted)
                }
            } else {
                None
            };
            Ok(SourcePage {
                items,
                source_epoch: None,
                done,
                done_reason,
            })
        }

        async fn close(&mut self) {}
    }

    #[async_trait]
    impl ReferenceSourceFactory for CountingFactory {
        async fn open(
            &self,
            request: &StartReferenceSearchRequest,
            _pattern: SearchPattern,
            limit: usize,
        ) -> Result<Box<dyn ReferenceSourceCursor>, AppCommandError> {
            Ok(Box::new(CountingCursor {
                remaining: self.candidate_count,
                produced: 0,
                limit,
                source: request.source,
                advances: Arc::clone(&self.advances),
            }))
        }
    }

    fn test_registry(limit: u16) -> Arc<ReferenceSearchRegistry> {
        let advances = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(CountingFactory {
            candidate_count: 0,
            advances: Arc::clone(&advances),
        });
        ReferenceSearchRegistry::new_with_counter(limit, factory, advances)
    }

    fn registry_with_cursor(candidate_count: usize) -> Arc<ReferenceSearchRegistry> {
        let advances = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(CountingFactory {
            candidate_count,
            advances: Arc::clone(&advances),
        });
        ReferenceSearchRegistry::new_with_counter(50, factory, advances)
    }

    fn workspace_for(source: ReferenceSearchSource) -> Option<String> {
        match source {
            ReferenceSearchSource::File | ReferenceSearchSource::Commit => {
                Some("workspace-root".to_string())
            }
            ReferenceSearchSource::Conversation => None,
        }
    }

    fn start_request(
        source: ReferenceSearchSource,
        sequence: u64,
        request_id: &str,
    ) -> StartReferenceSearchRequest {
        StartReferenceSearchRequest {
            search_session_id: UUID_A.to_string(),
            source_sequence: sequence,
            request_id: request_id.to_string(),
            source,
            query: "test".to_string(),
            workspace_path: workspace_for(source),
        }
    }

    fn unique_start(source: ReferenceSearchSource) -> StartReferenceSearchRequest {
        StartReferenceSearchRequest {
            search_session_id: Uuid::new_v4().hyphenated().to_string(),
            source_sequence: 1,
            request_id: Uuid::new_v4().hyphenated().to_string(),
            source,
            query: "test".to_string(),
            workspace_path: workspace_for(source),
        }
    }

    async fn seed_registered(
        registry: &Arc<ReferenceSearchRegistry>,
        source: ReferenceSearchSource,
        count: usize,
    ) {
        for _ in 0..count {
            registry
                .start(unique_start(source))
                .await
                .expect("seed start");
        }
    }

    async fn seed_cancel_guards(registry: &Arc<ReferenceSearchRegistry>, count: usize) {
        for _ in 0..count {
            let request = unique_start(ReferenceSearchSource::File);
            assert!(!registry
                .cancel(request.cancel_request())
                .await
                .expect("seed cancel"));
        }
    }

    fn assert_overloaded<T: std::fmt::Debug>(result: Result<T, AppCommandError>) {
        let error = result.expect_err("expected registry overload");
        assert_eq!(error.code, AppErrorCode::RegistryOverloaded);
    }

    fn assert_cancelled<T: std::fmt::Debug>(result: Result<T, AppCommandError>) {
        let error = result.expect_err("expected cancellation");
        assert_eq!(error.code, AppErrorCode::Cancelled);
    }

    fn synthetic_candidate(source: ReferenceSearchSource, ordinal: u64) -> ReferenceCandidate {
        match source {
            ReferenceSearchSource::File => ReferenceCandidate {
                source,
                uri: format!("file:///workspace/f{ordinal}.ts"),
                id: format!("f{ordinal}"),
                label: format!("f{ordinal}.ts"),
                detail: None,
                keywords: format!("f{ordinal}"),
                metadata: ReferenceCandidateMetadata::File {
                    canonical_workspace_root: "/workspace".to_string(),
                    relative_path: format!("f{ordinal}.ts"),
                    entry_kind: ReferenceFileKind::File,
                },
                source_ordinal: ordinal,
                regex_rank: None,
            },
            ReferenceSearchSource::Conversation => ReferenceCandidate {
                source,
                uri: format!("codeg://session/{ordinal}"),
                id: ordinal.to_string(),
                label: format!("conversation-{ordinal}"),
                detail: Some("completed".to_string()),
                keywords: format!("conversation-{ordinal}"),
                metadata: ReferenceCandidateMetadata::Conversation {
                    conversation_id: ordinal as i32 + 1,
                    agent_type: crate::models::agent::AgentType::ClaudeCode,
                    status: "completed".to_string(),
                    branch: None,
                    project_name: "proj".to_string(),
                    project_path: "/proj".to_string(),
                },
                source_ordinal: ordinal,
                regex_rank: None,
            },
            ReferenceSearchSource::Commit => ReferenceCandidate {
                source,
                uri: format!("codeg://commit/%2Frepo@hash{ordinal}"),
                id: format!("hash{ordinal}"),
                label: format!("hash{ordinal}"),
                detail: Some("subject".to_string()),
                keywords: format!("hash{ordinal}"),
                metadata: ReferenceCandidateMetadata::Commit {
                    canonical_repo: "/repo".to_string(),
                    full_hash: format!("hash{ordinal}"),
                    short_hash: format!("h{ordinal}"),
                    subject: "subject".to_string(),
                    message: "message".to_string(),
                    author: "author".to_string(),
                    authored_at: "2026-01-01T00:00:00Z".to_string(),
                },
                source_ordinal: ordinal,
                regex_rank: None,
            },
        }
    }

    /// Factory whose cursors block inside `next_page` until released.
    struct BlockedScanFactory {
        started: Arc<AtomicUsize>,
        started_by_source: Arc<Mutex<HashMap<ReferenceSearchSource, usize>>>,
        releases: Arc<Mutex<HashMap<ReferenceSearchSource, Vec<oneshot::Sender<()>>>>>,
    }

    struct BlockedCursor {
        source: ReferenceSearchSource,
        started: Arc<AtomicUsize>,
        started_by_source: Arc<Mutex<HashMap<ReferenceSearchSource, usize>>>,
        release_rx: Option<oneshot::Receiver<()>>,
    }

    #[async_trait]
    impl ReferenceSourceCursor for BlockedCursor {
        async fn next_page(
            &mut self,
            _page_size: usize,
            token: CancellationToken,
        ) -> Result<SourcePage, AppCommandError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            {
                let mut map = self.started_by_source.lock().await;
                *map.entry(self.source).or_insert(0) += 1;
            }
            if let Some(rx) = self.release_rx.take() {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(error_cancelled()),
                    result = rx => {
                        let _ = result;
                    }
                }
            }
            Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(ReferenceDoneReason::Exhausted),
            })
        }

        async fn close(&mut self) {}
    }

    #[async_trait]
    impl ReferenceSourceFactory for BlockedScanFactory {
        async fn open(
            &self,
            request: &StartReferenceSearchRequest,
            _pattern: SearchPattern,
            _limit: usize,
        ) -> Result<Box<dyn ReferenceSourceCursor>, AppCommandError> {
            let (tx, rx) = oneshot::channel();
            self.releases
                .lock()
                .await
                .entry(request.source)
                .or_default()
                .push(tx);
            Ok(Box::new(BlockedCursor {
                source: request.source,
                started: Arc::clone(&self.started),
                started_by_source: Arc::clone(&self.started_by_source),
                release_rx: Some(rx),
            }))
        }
    }

    struct BlockedScanFixture {
        registry: Arc<ReferenceSearchRegistry>,
        started: Arc<AtomicUsize>,
        started_by_source: Arc<Mutex<HashMap<ReferenceSearchSource, usize>>>,
        releases: Arc<Mutex<HashMap<ReferenceSearchSource, Vec<oneshot::Sender<()>>>>>,
    }

    async fn blocked_scan_fixture() -> BlockedScanFixture {
        let started = Arc::new(AtomicUsize::new(0));
        let started_by_source = Arc::new(Mutex::new(HashMap::new()));
        let releases = Arc::new(Mutex::new(HashMap::new()));
        let factory = Arc::new(BlockedScanFactory {
            started: Arc::clone(&started),
            started_by_source: Arc::clone(&started_by_source),
            releases: Arc::clone(&releases),
        });
        let registry = ReferenceSearchRegistry::new(50, factory);
        BlockedScanFixture {
            registry,
            started,
            started_by_source,
            releases,
        }
    }

    impl BlockedScanFixture {
        fn started_scan_count(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        async fn wait_for_started_scan_count(&self, target: usize) -> usize {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let count = self.started_scan_count();
                if count >= target {
                    return count;
                }
                if Instant::now() >= deadline {
                    panic!("timed out waiting for started scan count {target}, have {count}");
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        async fn wait_for_source_scan_count(
            &self,
            source: ReferenceSearchSource,
            target: usize,
        ) -> usize {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let count = {
                    let map = self.started_by_source.lock().await;
                    map.get(&source).copied().unwrap_or(0)
                };
                if count >= target {
                    return count;
                }
                if Instant::now() >= deadline {
                    panic!("timed out waiting for {source:?} scan count {target}, have {count}");
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        fn spawn_one_more(
            &self,
            source: ReferenceSearchSource,
        ) -> JoinHandle<Result<ReferenceSearchPage, AppCommandError>> {
            let registry = Arc::clone(&self.registry);
            let request = unique_start(source);
            tokio::spawn(async move { registry.start(request).await })
        }

        async fn start_distinct_sources_to_global_cap(&self, n: usize) {
            assert_eq!(n, 12, "fixture expects global scan cap of 12");
            let mut handles = Vec::new();
            for source in [
                ReferenceSearchSource::File,
                ReferenceSearchSource::Conversation,
                ReferenceSearchSource::Commit,
            ] {
                for _ in 0..4 {
                    handles.push(self.spawn_one_more(source));
                }
            }
            self.wait_for_started_scan_count(n).await;
            // Keep handles alive so tasks are not dropped mid-scan.
            self.park_handles(handles).await;
        }

        async fn start_source_to_scan_cap(&self, source: ReferenceSearchSource, n: usize) {
            let mut handles = Vec::new();
            for _ in 0..n {
                handles.push(self.spawn_one_more(source));
            }
            self.wait_for_source_scan_count(source, n).await;
            self.park_handles(handles).await;
        }

        async fn spawn_source_backlog(&self, source: ReferenceSearchSource, n: usize) {
            let mut handles = Vec::new();
            for _ in 0..n {
                handles.push(self.spawn_one_more(source));
            }
            self.wait_for_source_scan_count(source, MAX_SOURCE_SCANS)
                .await;
            self.park_handles(handles).await;
        }

        async fn release_one(&self, source: ReferenceSearchSource) {
            let tx = {
                let mut map = self.releases.lock().await;
                map.get_mut(&source)
                    .and_then(|queue| {
                        if queue.is_empty() {
                            None
                        } else {
                            Some(queue.remove(0))
                        }
                    })
                    .expect("release sender for source")
            };
            let _ = tx.send(());
        }

        async fn release_all(&self) {
            let mut map = self.releases.lock().await;
            for queue in map.values_mut() {
                for tx in queue.drain(..) {
                    let _ = tx.send(());
                }
            }
        }

        /// Store handles on the fixture via a side channel so they are not dropped.
        async fn park_handles(
            &self,
            handles: Vec<JoinHandle<Result<ReferenceSearchPage, AppCommandError>>>,
        ) {
            // Detach: spawn a janitor that awaits them after release_all in tests.
            // For backlog/cap setup we only need registrations + entered scans.
            // Keep them running by leaking the join handles into a static bag.
            for handle in handles {
                tokio::spawn(async move {
                    let _ = handle.await;
                });
            }
            // Yield so re-spawned waiters are scheduled.
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn pre_cancel_tombstone_beats_a_reordered_start() {
        let registry = test_registry(50);
        let request = start_request(ReferenceSearchSource::File, 4, UUID_A);
        assert!(!registry.cancel(request.cancel_request()).await.unwrap());
        let error = registry.start(request).await.expect_err("pre-cancelled");
        assert!(matches!(error.code, AppErrorCode::Cancelled));
        assert_eq!(registry.registered_count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_start_and_latest_page_share_or_replay_work() {
        let registry = registry_with_cursor(12);
        let start = start_request(ReferenceSearchSource::Conversation, 1, UUID_A);
        let (a, b) = tokio::join!(registry.start(start.clone()), registry.start(start.clone()));
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(registry.cursor_advance_count(), 1);
        let page1 = registry.next_page(start.next_request(1)).await.unwrap();
        let replay = registry.next_page(start.next_request(1)).await.unwrap();
        assert_eq!(page1, replay);
        assert_eq!(registry.cursor_advance_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn registered_and_scan_caps_are_enforced_exactly() {
        for (source, cap) in [
            (ReferenceSearchSource::File, 24),
            (ReferenceSearchSource::Conversation, 32),
            (ReferenceSearchSource::Commit, 8),
        ] {
            let registry = test_registry(50);
            seed_registered(&registry, source, cap).await;
            assert_overloaded(registry.start(unique_start(source)).await);
        }

        let registry = test_registry(50);
        seed_registered(&registry, ReferenceSearchSource::File, 24).await;
        seed_registered(&registry, ReferenceSearchSource::Conversation, 32).await;
        seed_registered(&registry, ReferenceSearchSource::Commit, 8).await;
        assert_eq!(registry.registered_count().await, 64);
        assert_overloaded(
            registry
                .start(unique_start(ReferenceSearchSource::File))
                .await,
        );

        let scans = blocked_scan_fixture().await;
        scans.start_distinct_sources_to_global_cap(12).await;
        assert_eq!(scans.started_scan_count(), 12);
        let mut thirteenth = scans.spawn_one_more(ReferenceSearchSource::Conversation);
        assert!(timeout(Duration::from_millis(20), &mut thirteenth)
            .await
            .is_err());
        scans.release_one(ReferenceSearchSource::Conversation).await;
        assert_eq!(scans.wait_for_started_scan_count(13).await, 13);
        scans.release_all().await;
        thirteenth.await.unwrap().unwrap();

        let per_source = blocked_scan_fixture().await;
        per_source
            .start_source_to_scan_cap(ReferenceSearchSource::Commit, 4)
            .await;
        let mut fifth = per_source.spawn_one_more(ReferenceSearchSource::Commit);
        assert!(timeout(Duration::from_millis(20), &mut fifth)
            .await
            .is_err());
        per_source.release_one(ReferenceSearchSource::Commit).await;
        assert_eq!(per_source.wait_for_started_scan_count(5).await, 5);
        per_source.release_all().await;
        fifth.await.unwrap().unwrap();

        let fairness = blocked_scan_fixture().await;
        fairness
            .spawn_source_backlog(ReferenceSearchSource::File, 12)
            .await;
        assert_eq!(
            fairness
                .wait_for_source_scan_count(ReferenceSearchSource::File, 4)
                .await,
            4
        );
        let conversation = fairness.spawn_one_more(ReferenceSearchSource::Conversation);
        fairness
            .wait_for_source_scan_count(ReferenceSearchSource::Conversation, 1)
            .await;
        assert!(!conversation.is_finished());
        fairness.release_all().await;
        conversation.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn guard_table_cap_rejects_new_identity_without_evicting_live_high_water() {
        let registry = test_registry(50);
        let retained = unique_start(ReferenceSearchSource::File);
        assert!(!registry.cancel(retained.cancel_request()).await.unwrap());
        seed_cancel_guards(&registry, 255).await;
        let overflow = unique_start(ReferenceSearchSource::Conversation);
        assert_overloaded(registry.cancel(overflow.cancel_request()).await);
        assert_cancelled(registry.start(retained).await);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_expiry_releases_the_job_but_retains_high_water() {
        let registry = registry_with_cursor(3);
        let request = start_request(ReferenceSearchSource::Conversation, 1, UUID_A);
        let page0 = registry.start(request.clone()).await.unwrap();
        assert!(page0.done);

        tokio::time::advance(Duration::from_secs(29)).await;
        let replay = registry.start(request.clone()).await.unwrap();
        assert_eq!(page0, replay);

        tokio::time::advance(Duration::from_secs(30)).await;
        registry.sweep_expired(Instant::now()).await;

        let expired = registry
            .start(request.clone())
            .await
            .expect_err("job expired");
        assert_eq!(expired.code, AppErrorCode::JobExpired);
        assert_eq!(registry.registered_count().await, 0);

        // Lower and equal sequences stay rejected while the five-minute guard lives.
        let lower = start_request(ReferenceSearchSource::Conversation, 1, UUID_A);
        let lower_err = registry
            .start(lower)
            .await
            .expect_err("equal still expired");
        assert_eq!(lower_err.code, AppErrorCode::JobExpired);

        let older = StartReferenceSearchRequest {
            source_sequence: 1,
            request_id: Uuid::new_v4().hyphenated().to_string(),
            ..start_request(ReferenceSearchSource::Conversation, 1, UUID_A)
        };
        // Different request_id at equal sequence is invalid, not a silent restart.
        let mismatch = registry.start(older).await.expect_err("equal seq mismatch");
        assert_eq!(mismatch.code, AppErrorCode::InvalidRequest);

        tokio::time::advance(GUARD_RETENTION).await;
        registry.sweep_expired(Instant::now()).await;

        // Exact identity can be admitted as new work only after the guard is gone.
        let revived = registry.start(request.clone()).await.unwrap();
        assert_eq!(revived.source_sequence, 1);
        assert_eq!(registry.registered_count().await, 1);
    }

    #[tokio::test]
    async fn limit_epoch_change_cancels_old_epoch_jobs_including_registration_race() {
        let scans = blocked_scan_fixture().await;
        let start = unique_start(ReferenceSearchSource::File);
        let registry = Arc::clone(&scans.registry);

        let blocked = {
            let registry = Arc::clone(&registry);
            let start = start.clone();
            tokio::spawn(async move { registry.start(start).await })
        };
        scans.wait_for_started_scan_count(1).await;

        let epoch = registry.set_limit(25).await;
        assert!(epoch >= 1);

        let error = blocked.await.unwrap().expect_err("limit epoch");
        assert_eq!(error.code, AppErrorCode::LimitEpochChanged);

        // Exact equal-sequence retry replays the guard error without a slot.
        let replay = registry
            .start(start.clone())
            .await
            .expect_err("replay epoch");
        assert_eq!(replay.code, AppErrorCode::LimitEpochChanged);
        assert_eq!(registry.registered_count().await, 0);

        // Higher sequence may start under the new epoch.
        let higher = StartReferenceSearchRequest {
            source_sequence: start.source_sequence + 1,
            request_id: Uuid::new_v4().hyphenated().to_string(),
            ..start
        };
        let higher_handle = {
            let registry = Arc::clone(&registry);
            tokio::spawn(async move { registry.start(higher).await })
        };
        // Previous blocked cursor may still hold a release sender; the new
        // start also needs a release after it enters next_page.
        let _ = scans.wait_for_started_scan_count(2).await;
        scans.release_all().await;
        let page = higher_handle.await.unwrap().unwrap();
        assert!(page.done);
    }
}
