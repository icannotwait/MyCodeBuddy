//! Host-only request-scoped wait cancel registry.
//!
//! Wakes a single parked `get_delegation_status` / Join wait via a watch channel.
//! Never cancels child Broker tasks. Full [`WaitStamp`] validation prevents
//! stale wait_id reuse across incarnation, turn, or parent identity.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::acp::tool_watchdog::{
    CancelCause, WaitCancelHandle, WaitCancelResult, WaitOwner, WaitStamp,
};

/// Progress routing target for a live wait with a concrete wait tool call id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitProgressTarget {
    pub wait_id: String,
    pub wait_tool_call_id: String,
}

/// Trim, drop empty, and de-dupe task ids while preserving first-seen order.
pub fn normalize_wait_task_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        if out.iter().any(|existing| existing == trimmed) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

/// Host-only registry of parked multi-task wait cancel handles.
#[derive(Default)]
pub struct WaitCancelRegistry {
    inner: Mutex<HashMap<String, RegisteredWait>>,
    /// Test-only: park arm tasks after `transfer_owner` succeeds and before
    /// `transfer_tx.send`, so peer-close can race the handoff window.
    #[cfg(any(test, feature = "test-utils"))]
    transfer_handoff_gate: Mutex<Option<TransferHandoffGate>>,
}

impl std::fmt::Debug for WaitCancelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitCancelRegistry")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Oneshot pair: entered signal + release barrier for transfer handoff tests.
#[cfg(any(test, feature = "test-utils"))]
struct TransferHandoffGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Debug)]
struct RegisteredWait {
    stamp: WaitStamp,
    owner: WaitOwner,
    /// `None` until cancelled; then carries the initiating cause so the
    /// waiter can emit `tool_stalled_timeout` vs `user_cancelled`.
    cancel: tokio::sync::watch::Sender<Option<CancelCause>>,
    /// Canonical awaited task ids (normalized). Exact membership only.
    task_ids: Vec<String>,
    settled: bool,
}

impl WaitCancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Register before parking the join. Stamp includes incarnation+turn+parent.
    ///
    /// Returns `AlreadySettled` if the same wait_id is still present after settle
    /// (should not happen — callers deregister). Returns `Stale` if wait_id is
    /// already live with a different stamp.
    pub async fn register(&self, handle: WaitCancelHandle) -> Result<(), WaitCancelResult> {
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.get(&handle.stamp.wait_id) {
            if existing.settled {
                return Err(WaitCancelResult::AlreadySettled);
            }
            if existing.stamp != handle.stamp {
                return Err(WaitCancelResult::Stale);
            }
            // Idempotent re-register of the same live stamp is a no-op success.
            return Ok(());
        }
        inner.insert(
            handle.stamp.wait_id.clone(),
            RegisteredWait {
                stamp: handle.stamp,
                owner: handle.owner,
                cancel: handle.cancel,
                task_ids: normalize_wait_task_ids(&handle.task_ids),
                settled: false,
            },
        );
        Ok(())
    }

    /// Atomic transfer when continuation takes ownership of the wait
    /// (`JoinArmOutcome::Arming` / Suspended handoff).
    pub async fn transfer_owner(
        &self,
        wait_id: &str,
        expected: &WaitStamp,
        new_owner: WaitOwner,
    ) -> Result<(), WaitCancelResult> {
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get_mut(wait_id) else {
            return Err(WaitCancelResult::NotFound);
        };
        if entry.settled {
            return Err(WaitCancelResult::AlreadySettled);
        }
        if &entry.stamp != expected {
            return Err(WaitCancelResult::Stale);
        }
        entry.owner = new_owner;
        Ok(())
    }

    /// Cancel wakes only this wait via watch; never cancels child tasks.
    /// Validates full [`WaitStamp`] (incarnation, turn, parent), not wait_id alone.
    pub async fn cancel(
        &self,
        expected: &WaitStamp,
        cause: CancelCause,
    ) -> WaitCancelResult {
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get_mut(&expected.wait_id) else {
            return WaitCancelResult::NotFound;
        };
        if entry.settled {
            return WaitCancelResult::AlreadySettled;
        }
        if &entry.stamp != expected {
            return WaitCancelResult::Stale;
        }
        let _ = entry.cancel.send(Some(cause));
        entry.settled = true;
        WaitCancelResult::Cancelled
    }

    /// Peer-close / normal completion / disconnect must deregister.
    pub async fn deregister(&self, expected: &WaitStamp) -> WaitCancelResult {
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get(&expected.wait_id) else {
            return WaitCancelResult::NotFound;
        };
        if &entry.stamp != expected {
            return WaitCancelResult::Stale;
        }
        let was_settled = entry.settled;
        inner.remove(&expected.wait_id);
        if was_settled {
            WaitCancelResult::AlreadySettled
        } else {
            WaitCancelResult::Cancelled
        }
    }

    /// Drop-path deregister that is ownership-linearizable with
    /// [`Self::transfer_owner`].
    ///
    /// After a successful transfer to [`WaitOwner::ContinuationCoordinator`],
    /// a late listener `WaitCancelGuard` Drop must not remove the entry: the
    /// coordinator / [`TransferredWait`] owns cleanup. Owner mismatch is a
    /// no-op (returns [`WaitCancelResult::Stale`]).
    pub async fn deregister_if_owner(
        &self,
        expected: &WaitStamp,
        owner: WaitOwner,
    ) -> WaitCancelResult {
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get(&expected.wait_id) else {
            return WaitCancelResult::NotFound;
        };
        if &entry.stamp != expected {
            return WaitCancelResult::Stale;
        }
        if entry.owner != owner {
            return WaitCancelResult::Stale;
        }
        let was_settled = entry.settled;
        inner.remove(&expected.wait_id);
        if was_settled {
            WaitCancelResult::AlreadySettled
        } else {
            WaitCancelResult::Cancelled
        }
    }

    /// Install a one-shot barrier after `transfer_owner` and before handoff send.
    ///
    /// Returns `(entered_rx, release_tx)`. The arm path signals `entered` then
    /// waits for `release` before `transfer_tx.send`.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_transfer_handoff_gate(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.transfer_handoff_gate.lock().await = Some(TransferHandoffGate {
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }

    /// Observe the transfer handoff gate if installed (test-utils only).
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn observe_transfer_handoff_gate(&self) {
        let gate = self.transfer_handoff_gate.lock().await.take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }

    /// Current owner of a live wait (test / host inspection).
    pub async fn owner(&self, wait_id: &str) -> Option<WaitOwner> {
        let inner = self.inner.lock().await;
        inner
            .get(wait_id)
            .filter(|e| !e.settled)
            .map(|e| e.owner)
    }

    /// Whether a wait_id is currently registered (live or settled-until-deregister).
    pub async fn contains(&self, wait_id: &str) -> bool {
        self.inner.lock().await.contains_key(wait_id)
    }

    /// Live (not settled) wait stamps — test/inspection only.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn live_wait_stamps(&self) -> Vec<WaitStamp> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| !e.settled)
            .map(|e| e.stamp.clone())
            .collect()
    }

    /// Live task_ids for a wait_id — test/inspection only.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn live_task_ids(&self, wait_id: &str) -> Option<Vec<String>> {
        self.inner.lock().await.get(wait_id).and_then(|e| {
            if e.settled {
                None
            } else {
                Some(e.task_ids.clone())
            }
        })
    }

    /// Read-only exact-match of a child task against live waits.
    ///
    /// A wait matches only when it is live (not settled), the task id is a
    /// member of its normalized `task_ids`, connection id + incarnation +
    /// turn generation match, and `parent_tool_use_id` is a concrete (non-blank)
    /// wait tool call id. Never invents tool ids.
    ///
    /// Wait tool call ids keep **original host bytes** (trim only rejects blank)
    /// so progress renew keys match bind/lease lookup (which also uses raw bytes).
    pub async fn exact_match_progress_targets(
        &self,
        task_id: &str,
        connection_id: &str,
        connection_incarnation: &str,
        turn_generation: u64,
    ) -> Vec<WaitProgressTarget> {
        let inner = self.inner.lock().await;
        let mut targets = Vec::new();
        for entry in inner.values() {
            if entry.settled {
                continue;
            }
            if entry.stamp.connection_id != connection_id {
                continue;
            }
            if entry.stamp.connection_incarnation != connection_incarnation {
                continue;
            }
            if entry.stamp.turn_generation != turn_generation {
                continue;
            }
            if !entry.task_ids.iter().any(|id| id == task_id) {
                continue;
            }
            // Trim only to reject blank; preserve opaque host bytes for renew.
            let Some(wait_tool_call_id) = entry
                .stamp
                .parent_tool_use_id
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
            else {
                continue;
            };
            targets.push(WaitProgressTarget {
                wait_id: entry.stamp.wait_id.clone(),
                wait_tool_call_id,
            });
        }
        targets
    }
}

/// Build a cancelable watch pair for a new wait registration.
///
/// Initial value is `None` (not cancelled). Host cancel writes `Some(cause)`.
pub fn new_wait_cancel_channel() -> (
    tokio::sync::watch::Sender<Option<CancelCause>>,
    tokio::sync::watch::Receiver<Option<CancelCause>>,
) {
    tokio::sync::watch::channel(None)
}

/// True when the watch receiver has observed a cancel cause.
pub fn cancel_flag_set(rx: &tokio::sync::watch::Receiver<Option<CancelCause>>) -> bool {
    rx.borrow().is_some()
}

/// Observed cancel cause, if any.
pub fn cancel_cause_of(
    rx: &tokio::sync::watch::Receiver<Option<CancelCause>>,
) -> Option<CancelCause> {
    *rx.borrow()
}

/// Drop guard that deregisters a wait when the parking task is abandoned
/// (peer-close, task cancel, etc.). Explicit completion should call
/// [`WaitCancelGuard::disarm`] after a successful `deregister`.
///
/// Drop only removes entries still owned by [`WaitOwner::Listener`]. After a
/// successful `transfer_owner` to the continuation coordinator, Drop is a
/// no-op even if `drop_armed` is still true — handoff is linearizable at
/// transfer, not only after `transfer_tx.send`.
///
/// After a successful handoff send, call
/// [`WaitCancelGuard::drop_armed_flag`]`().store(false, …)` from the transfer
/// task so peer-close Drop is also a fast no-op.
pub struct WaitCancelGuard {
    registry: Arc<WaitCancelRegistry>,
    stamp: Option<WaitStamp>,
    /// Shared latch: when false, [`Drop`] skips deregister even if `stamp` is
    /// still `Some`. Lets the arm/transfer task disarm without `&mut` on the
    /// listener-owned guard.
    drop_armed: Arc<std::sync::atomic::AtomicBool>,
}

impl WaitCancelGuard {
    pub fn new(registry: Arc<WaitCancelRegistry>, stamp: WaitStamp) -> Self {
        Self {
            registry,
            stamp: Some(stamp),
            drop_armed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// Stop Drop from re-deregistering after an explicit cleanup path.
    pub fn disarm(&mut self) {
        self.stamp = None;
        self.drop_armed
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Cloneable latch for cross-task disarm after ownership transfer.
    ///
    /// Store `false` after a successful `transfer_tx.send` so peer-close that
    /// drops the listener future cannot Drop-deregister the coordinator-owned
    /// registration.
    pub fn drop_armed_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.drop_armed.clone()
    }
}

impl Drop for WaitCancelGuard {
    fn drop(&mut self) {
        if !self
            .drop_armed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let Some(stamp) = self.stamp.take() else {
            return;
        };
        let registry = self.registry.clone();
        // Async-safe cleanup: peer-close abandons `process_status` without an
        // explicit deregister await. Spawn on the current runtime when present.
        // Only Listener-owned rows are removed — post-transfer coordinator
        // ownership is preserved for the residual transfer→send window.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = registry
                    .deregister_if_owner(&stamp, WaitOwner::Listener)
                    .await;
            });
        }
    }
}

/// Transferable wait ownership for continuation handoff.
///
/// Private fields; [`Drop`] deregisters the wait if still armed so abandoned
/// suspended waits leave no ownerless registry entry.
///
/// Also watches [`cancel_rx`] and `waiter_closed`: either signal deregisters
/// the registration **without** cancelling the durable continuation worker.
/// Status peer-close cancels `waiter_closed` only (no cancel cause).
pub struct TransferredWait {
    stamp: WaitStamp,
    task_ids: Vec<String>,
    cancel_rx: tokio::sync::watch::Receiver<Option<CancelCause>>,
    registry: Arc<WaitCancelRegistry>,
    /// When true, Drop will deregister. Cleared after explicit successful
    /// deregister via [`TransferredWait::disarm_cleanup`].
    armed: bool,
    /// Abort handle for the background deregister watch (cancel / peer-close).
    cleanup_abort: Option<tokio::task::AbortHandle>,
}

impl TransferredWait {
    /// Build ownership after a successful `transfer_owner` to the coordinator.
    ///
    /// `waiter_closed` is the status-request liveness token: peer-close /
    /// abandonment cancels it and must deregister the wait registration while
    /// leaving the continuation worker running.
    pub fn new(
        stamp: WaitStamp,
        task_ids: Vec<String>,
        cancel_rx: tokio::sync::watch::Receiver<Option<CancelCause>>,
        registry: Arc<WaitCancelRegistry>,
        waiter_closed: tokio_util::sync::CancellationToken,
    ) -> Self {
        let cleanup_abort =
            Self::spawn_registration_cleanup(stamp.clone(), cancel_rx.clone(), registry.clone(), waiter_closed);
        Self {
            stamp,
            task_ids,
            cancel_rx,
            registry,
            armed: true,
            cleanup_abort: Some(cleanup_abort),
        }
    }

    /// Deregister on wait cancel cause **or** status waiter abandonment.
    /// Never cancels Broker children or the continuation worker token.
    fn spawn_registration_cleanup(
        stamp: WaitStamp,
        mut cancel_rx: tokio::sync::watch::Receiver<Option<CancelCause>>,
        registry: Arc<WaitCancelRegistry>,
        waiter_closed: tokio_util::sync::CancellationToken,
    ) -> tokio::task::AbortHandle {
        let handle = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = waiter_closed.cancelled() => {
                    let _ = registry.deregister(&stamp).await;
                }
                _ = async {
                    loop {
                        if cancel_flag_set(&cancel_rx) {
                            break;
                        }
                        if cancel_rx.changed().await.is_err() {
                            break;
                        }
                    }
                } => {
                    let _ = registry.deregister(&stamp).await;
                }
            }
        });
        handle.abort_handle()
    }

    pub fn stamp(&self) -> &WaitStamp {
        &self.stamp
    }

    pub fn task_ids(&self) -> &[String] {
        &self.task_ids
    }

    pub fn cancel_rx(&mut self) -> &mut tokio::sync::watch::Receiver<Option<CancelCause>> {
        &mut self.cancel_rx
    }

    /// After explicit successful deregister — Drop must not double-deregister.
    pub fn disarm_cleanup(&mut self) {
        self.armed = false;
        if let Some(handle) = self.cleanup_abort.take() {
            handle.abort();
        }
    }
}

impl Drop for TransferredWait {
    fn drop(&mut self) {
        if let Some(handle) = self.cleanup_abort.take() {
            handle.abort();
        }
        if !self.armed {
            return;
        }
        self.armed = false;
        let stamp = self.stamp.clone();
        let registry = self.registry.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = registry.deregister(&stamp).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::tool_watchdog::WaitOwner;
    use tokio_util::sync::CancellationToken;

    fn stamp(wait_id: &str) -> WaitStamp {
        WaitStamp {
            wait_id: wait_id.into(),
            connection_id: "conn-1".into(),
            connection_incarnation: "inc-1".into(),
            turn_generation: 3,
            parent_conversation_id: 42,
            parent_tool_use_id: Some("tool-wait".into()),
        }
    }

    fn handle(
        wait_id: &str,
        owner: WaitOwner,
    ) -> (
        WaitCancelHandle,
        tokio::sync::watch::Receiver<Option<CancelCause>>,
    ) {
        handle_with_tasks(wait_id, owner, vec![])
    }

    fn handle_with_tasks(
        wait_id: &str,
        owner: WaitOwner,
        task_ids: Vec<String>,
    ) -> (
        WaitCancelHandle,
        tokio::sync::watch::Receiver<Option<CancelCause>>,
    ) {
        let (tx, rx) = new_wait_cancel_channel();
        (
            WaitCancelHandle {
                stamp: stamp(wait_id),
                owner,
                cancel: tx,
                task_ids,
            },
            rx,
        )
    }

    fn target(wait_id: &str, wait_tool_call_id: &str) -> WaitProgressTarget {
        WaitProgressTarget {
            wait_id: wait_id.into(),
            wait_tool_call_id: wait_tool_call_id.into(),
        }
    }

    #[tokio::test]
    async fn cancel_wakes_only_matching_stamp() {
        let reg = WaitCancelRegistry::new();
        let (h, mut rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();

        let wrong = WaitStamp {
            turn_generation: 99,
            ..stamp("w1")
        };
        assert_eq!(
            reg.cancel(&wrong, CancelCause::AutoTimeout).await,
            WaitCancelResult::Stale
        );
        assert!(!cancel_flag_set(&rx));

        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::Cancelled
        );
        assert!(cancel_flag_set(&rx));
        assert_eq!(cancel_cause_of(&rx), Some(CancelCause::AutoTimeout));
        // Receiver observes change.
        let _ = rx.changed().await;
        assert!(rx.borrow().is_some());
    }

    #[tokio::test]
    async fn cancel_never_guesses_other_wait_ids() {
        let reg = WaitCancelRegistry::new();
        let (h1, rx1) = handle("w1", WaitOwner::Listener);
        let (h2, rx2) = handle("w2", WaitOwner::Listener);
        reg.register(h1).await.unwrap();
        reg.register(h2).await.unwrap();

        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::Cancelled
        );
        assert!(cancel_flag_set(&rx1));
        assert!(!cancel_flag_set(&rx2));
    }

    #[tokio::test]
    async fn transfer_owner_to_continuation_then_cancel() {
        let reg = WaitCancelRegistry::new();
        let (h, rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        assert_eq!(reg.owner("w1").await, Some(WaitOwner::Listener));

        reg.transfer_owner("w1", &stamp("w1"), WaitOwner::ContinuationCoordinator)
            .await
            .unwrap();
        assert_eq!(
            reg.owner("w1").await,
            Some(WaitOwner::ContinuationCoordinator)
        );

        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::UserStop).await,
            WaitCancelResult::Cancelled
        );
        assert!(cancel_flag_set(&rx));
        assert_eq!(cancel_cause_of(&rx), Some(CancelCause::UserStop));
    }

    #[tokio::test]
    async fn normal_completion_deregister_then_cancel_is_not_found() {
        let reg = WaitCancelRegistry::new();
        let (h, rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        // Normal completion deregisters without firing cancel.
        assert_eq!(
            reg.deregister(&stamp("w1")).await,
            WaitCancelResult::Cancelled
        );
        assert!(!cancel_flag_set(&rx));
        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::NotFound
        );
    }

    #[tokio::test]
    async fn peer_close_deregister_then_cancel_is_not_found() {
        let reg = WaitCancelRegistry::new();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        let _ = reg.deregister(&stamp("w1")).await;
        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::NotFound
        );
    }

    #[tokio::test]
    async fn peer_close_drop_guard_deregisters_async() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        {
            let _guard = WaitCancelGuard::new(reg.clone(), stamp("w1"));
            // Drop without disarm simulates peer-close abandoning the waiter.
        }
        // Yield so the Drop-spawned deregister task can run.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::NotFound
        );
    }

    #[tokio::test]
    async fn drop_armed_flag_false_skips_drop_deregister() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        {
            let guard = WaitCancelGuard::new(reg.clone(), stamp("w1"));
            // Simulate transfer path: clear latch without calling disarm(&mut).
            guard
                .drop_armed_flag()
                .store(false, std::sync::atomic::Ordering::SeqCst);
            // Drop must not deregister coordinator-owned wait.
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            reg.contains("w1").await,
            "drop_armed=false must skip Drop deregister"
        );
        let _ = reg.deregister(&stamp("w1")).await;
    }

    /// Residual race: transfer_owner flipped ownership, but transfer_tx.send has
    /// not run yet and drop_armed is still true. Listener Drop must not remove
    /// the coordinator-owned registration.
    #[tokio::test]
    async fn drop_after_transfer_owner_before_send_preserves_coordinator_wait() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        reg.transfer_owner("w1", &stamp("w1"), WaitOwner::ContinuationCoordinator)
            .await
            .unwrap();
        {
            // drop_armed still true — models residual Arming→send window.
            let _guard = WaitCancelGuard::new(reg.clone(), stamp("w1"));
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            reg.contains("w1").await,
            "owner-aware Drop must not remove post-transfer coordinator wait"
        );
        assert_eq!(
            reg.owner("w1").await,
            Some(WaitOwner::ContinuationCoordinator)
        );
        assert_eq!(
            reg.deregister_if_owner(&stamp("w1"), WaitOwner::Listener)
                .await,
            WaitCancelResult::Stale,
            "Listener Drop path must no-op after transfer"
        );
        assert!(reg.contains("w1").await);
        let _ = reg.deregister(&stamp("w1")).await;
    }

    #[tokio::test]
    async fn cancel_then_deregister_reports_already_settled() {
        let reg = WaitCancelRegistry::new();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::Cancelled
        );
        assert_eq!(
            reg.deregister(&stamp("w1")).await,
            WaitCancelResult::AlreadySettled
        );
        assert_eq!(
            reg.cancel(&stamp("w1"), CancelCause::AutoTimeout).await,
            WaitCancelResult::NotFound
        );
    }

    #[tokio::test]
    async fn transfer_stale_stamp_rejected() {
        let reg = WaitCancelRegistry::new();
        let (h, _rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        let stale = WaitStamp {
            connection_incarnation: "other".into(),
            ..stamp("w1")
        };
        assert_eq!(
            reg.transfer_owner("w1", &stale, WaitOwner::ContinuationCoordinator)
                .await,
            Err(WaitCancelResult::Stale)
        );
        assert_eq!(reg.owner("w1").await, Some(WaitOwner::Listener));
    }

    #[tokio::test]
    async fn multi_task_wait_cancel_leaves_siblings_unrelated() {
        // Registry-level guarantee: cancel is wait_id scoped; sibling waits
        // stay parked. Child tasks are never referenced here.
        let reg = WaitCancelRegistry::new();
        let (active, rx_active) = handle("wait-active", WaitOwner::Listener);
        let (unrelated, rx_unrelated) = handle("wait-unrelated", WaitOwner::Listener);
        reg.register(active).await.unwrap();
        reg.register(unrelated).await.unwrap();

        assert_eq!(
            reg.cancel(&stamp("wait-active"), CancelCause::AutoTimeout)
                .await,
            WaitCancelResult::Cancelled
        );
        assert!(cancel_flag_set(&rx_active));
        assert!(!cancel_flag_set(&rx_unrelated));
        assert_eq!(reg.owner("wait-unrelated").await, Some(WaitOwner::Listener));
    }

    #[tokio::test]
    async fn full_stamp_requires_parent_tool_identity() {
        let reg = WaitCancelRegistry::new();
        let (h, rx) = handle("w1", WaitOwner::Listener);
        reg.register(h).await.unwrap();
        let reduced = WaitStamp {
            parent_tool_use_id: None,
            ..stamp("w1")
        };
        assert_eq!(
            reg.cancel(&reduced, CancelCause::AutoTimeout).await,
            WaitCancelResult::Stale
        );
        assert!(!cancel_flag_set(&rx));
    }

    /// Whitespace-padded wait tool ids must keep original host bytes for renew
    /// (bind uses raw lease keys; trim only rejects blank).
    #[tokio::test]
    async fn exact_match_preserves_whitespace_padded_wait_tool_id_bytes() {
        let reg = WaitCancelRegistry::new();
        let padded = "  wait-tool-padded  ";
        let (tx, _rx) = new_wait_cancel_channel();
        let mut s = stamp("wait-padded");
        s.parent_tool_use_id = Some(padded.into());
        reg.register(WaitCancelHandle {
            stamp: s,
            owner: WaitOwner::Listener,
            cancel: tx,
            task_ids: vec!["task-pad".into()],
        })
        .await
        .unwrap();

        let targets = reg
            .exact_match_progress_targets("task-pad", "conn-1", "inc-1", 3)
            .await;
        assert_eq!(
            targets,
            vec![target("wait-padded", padded)],
            "exact_match must not trim wait tool id bytes used by bind/lease lookup"
        );
        assert_ne!(
            targets[0].wait_tool_call_id.as_str(),
            padded.trim(),
            "trimmed id would miss the bound lease key"
        );
    }

    #[tokio::test]
    async fn exact_match_member_live_concrete_tool() {
        let reg = WaitCancelRegistry::new();

        // Singleton membership.
        let (h_solo, _) = handle_with_tasks(
            "wait-solo",
            WaitOwner::Listener,
            vec!["task-a".into()],
        );
        reg.register(h_solo).await.unwrap();
        assert_eq!(
            reg.exact_match_progress_targets("task-a", "conn-1", "inc-1", 3)
                .await,
            vec![target("wait-solo", "tool-wait")]
        );

        // Multi-task membership: each member matches the same wait once.
        let (h_multi, _) = handle_with_tasks(
            "wait-multi",
            WaitOwner::Listener,
            vec!["task-x".into(), "task-y".into(), "task-z".into()],
        );
        reg.register(h_multi).await.unwrap();
        for member in ["task-x", "task-y", "task-z"] {
            assert_eq!(
                reg.exact_match_progress_targets(member, "conn-1", "inc-1", 3)
                    .await,
                vec![target("wait-multi", "tool-wait")]
            );
        }

        // Two live waits sharing a member both surface as targets.
        let (h_share, _) = handle_with_tasks(
            "wait-share",
            WaitOwner::Listener,
            vec!["task-a".into(), "task-b".into()],
        );
        reg.register(h_share).await.unwrap();
        let mut shared = reg
            .exact_match_progress_targets("task-a", "conn-1", "inc-1", 3)
            .await;
        shared.sort_by(|a, b| a.wait_id.cmp(&b.wait_id));
        assert_eq!(
            shared,
            vec![
                target("wait-share", "tool-wait"),
                target("wait-solo", "tool-wait"),
            ]
        );
    }

    #[tokio::test]
    async fn exact_match_rejects_outsider_settled_stale_turn_stale_incarnation_blank_tool() {
        let reg = WaitCancelRegistry::new();
        let (h, _) = handle_with_tasks(
            "wait-live",
            WaitOwner::Listener,
            vec!["task-in".into()],
        );
        reg.register(h).await.unwrap();

        // Outsider task is not a member.
        assert!(
            reg.exact_match_progress_targets("task-out", "conn-1", "inc-1", 3)
                .await
                .is_empty()
        );

        // Stale turn generation.
        assert!(
            reg.exact_match_progress_targets("task-in", "conn-1", "inc-1", 99)
                .await
                .is_empty()
        );

        // Stale connection incarnation.
        assert!(
            reg.exact_match_progress_targets("task-in", "conn-1", "inc-other", 3)
                .await
                .is_empty()
        );

        // Wrong connection id.
        assert!(
            reg.exact_match_progress_targets("task-in", "conn-other", "inc-1", 3)
                .await
                .is_empty()
        );

        // Settled waits do not match.
        assert_eq!(
            reg.cancel(&stamp("wait-live"), CancelCause::AutoTimeout)
                .await,
            WaitCancelResult::Cancelled
        );
        assert!(
            reg.exact_match_progress_targets("task-in", "conn-1", "inc-1", 3)
                .await
                .is_empty()
        );

        // Deregistered waits do not match.
        assert_eq!(
            reg.deregister(&stamp("wait-live")).await,
            WaitCancelResult::AlreadySettled
        );
        assert!(
            reg.exact_match_progress_targets("task-in", "conn-1", "inc-1", 3)
                .await
                .is_empty()
        );

        // Missing / blank parent_tool_use_id yields no targets (no invent).
        for blank in [None, Some(String::new()), Some("   ".into())] {
            let wait_id = match &blank {
                None => "wait-blank-none",
                Some(v) if v.is_empty() => "wait-blank-empty",
                Some(_) => "wait-blank-ws",
            };
            let (tx, _rx) = new_wait_cancel_channel();
            let mut s = stamp(wait_id);
            s.parent_tool_use_id = blank;
            reg.register(WaitCancelHandle {
                stamp: s,
                owner: WaitOwner::Listener,
                cancel: tx,
                task_ids: vec!["task-blank".into()],
            })
            .await
            .unwrap();
            assert!(
                reg.exact_match_progress_targets("task-blank", "conn-1", "inc-1", 3)
                    .await
                    .is_empty(),
                "blank/missing parent_tool_use_id must not match ({wait_id})"
            );
        }
    }

    #[tokio::test]
    async fn transfer_owner_preserves_task_ids_and_stamp() {
        let reg = WaitCancelRegistry::new();
        let (h, _) = handle_with_tasks(
            "wait-xfer",
            WaitOwner::Listener,
            vec!["task-1".into(), "task-2".into()],
        );
        reg.register(h).await.unwrap();

        reg.transfer_owner(
            "wait-xfer",
            &stamp("wait-xfer"),
            WaitOwner::ContinuationCoordinator,
        )
        .await
        .unwrap();

        assert_eq!(
            reg.owner("wait-xfer").await,
            Some(WaitOwner::ContinuationCoordinator)
        );
        // Exact-match still works: task set and stamp identity preserved.
        assert_eq!(
            reg.exact_match_progress_targets("task-1", "conn-1", "inc-1", 3)
                .await,
            vec![target("wait-xfer", "tool-wait")]
        );
        assert_eq!(
            reg.exact_match_progress_targets("task-2", "conn-1", "inc-1", 3)
                .await,
            vec![target("wait-xfer", "tool-wait")]
        );
        // Full stamp still required for cancel after transfer.
        assert_eq!(
            reg.cancel(&stamp("wait-xfer"), CancelCause::UserStop).await,
            WaitCancelResult::Cancelled
        );
    }

    #[tokio::test]
    async fn transferred_wait_drop_deregisters_when_armed() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, rx) = handle_with_tasks(
            "wait-drop",
            WaitOwner::Listener,
            vec!["task-1".into()],
        );
        let stamp = h.stamp.clone();
        reg.register(h).await.unwrap();
        reg.transfer_owner(
            "wait-drop",
            &stamp,
            WaitOwner::ContinuationCoordinator,
        )
        .await
        .unwrap();

        {
            let transferred = TransferredWait::new(
                stamp.clone(),
                vec!["task-1".into()],
                rx,
                reg.clone(),
                CancellationToken::new(),
            );
            assert!(reg.contains("wait-drop").await);
            drop(transferred);
        }
        // Async Drop path — allow spawn to run.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !reg.contains("wait-drop").await,
            "armed TransferredWait Drop must deregister"
        );
    }

    /// Status peer-close / waiter abandonment after transfer must remove the
    /// registration without writing a cancel cause (continuation stays durable).
    #[tokio::test]
    async fn transferred_wait_waiter_closed_deregisters_without_cancel_cause() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, rx) = handle_with_tasks(
            "wait-waiter-closed",
            WaitOwner::Listener,
            vec!["task-1".into()],
        );
        let stamp = h.stamp.clone();
        reg.register(h).await.unwrap();
        reg.transfer_owner(
            "wait-waiter-closed",
            &stamp,
            WaitOwner::ContinuationCoordinator,
        )
        .await
        .unwrap();

        let waiter_closed = CancellationToken::new();
        let transferred = TransferredWait::new(
            stamp.clone(),
            vec!["task-1".into()],
            rx.clone(),
            reg.clone(),
            waiter_closed.clone(),
        );
        assert!(reg.contains("wait-waiter-closed").await);

        // Peer-close of the status request cancels waiter_closed only.
        waiter_closed.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while reg.contains("wait-waiter-closed").await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiter_closed must deregister transferred wait");

        assert!(
            !cancel_flag_set(&rx),
            "peer-close deregister must not send a cancel cause"
        );
        // Hold transferred so Drop is not what cleaned up.
        drop(transferred);
    }

    /// Host/wait cancel after transfer must also deregister (consume cancel_rx).
    #[tokio::test]
    async fn transferred_wait_cancel_rx_deregisters_registration() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, rx) = handle_with_tasks(
            "wait-cancel-consume",
            WaitOwner::Listener,
            vec!["task-1".into()],
        );
        let stamp = h.stamp.clone();
        reg.register(h).await.unwrap();
        reg.transfer_owner(
            "wait-cancel-consume",
            &stamp,
            WaitOwner::ContinuationCoordinator,
        )
        .await
        .unwrap();

        let transferred = TransferredWait::new(
            stamp.clone(),
            vec!["task-1".into()],
            rx,
            reg.clone(),
            CancellationToken::new(),
        );

        assert_eq!(
            reg.cancel(&stamp, CancelCause::AutoTimeout).await,
            WaitCancelResult::Cancelled
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while reg.contains("wait-cancel-consume").await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancel_rx consumer must deregister after cancel");
        drop(transferred);
    }

    #[tokio::test]
    async fn transferred_wait_disarm_skips_drop_deregister() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, rx) = handle_with_tasks(
            "wait-disarm",
            WaitOwner::Listener,
            vec!["task-1".into()],
        );
        let stamp = h.stamp.clone();
        reg.register(h).await.unwrap();
        {
            let mut transferred = TransferredWait::new(
                stamp.clone(),
                vec!["task-1".into()],
                rx,
                reg.clone(),
                CancellationToken::new(),
            );
            // Explicit deregister then disarm — Drop must not double-remove.
            assert_eq!(
                reg.deregister(transferred.stamp()).await,
                WaitCancelResult::Cancelled
            );
            transferred.disarm_cleanup();
            drop(transferred);
        }
        tokio::task::yield_now().await;
        assert!(!reg.contains("wait-disarm").await);
    }

    #[tokio::test]
    async fn transfer_oneshot_success_delivers_ownership() {
        let reg = WaitCancelRegistry::new_shared();
        let (h, rx) = handle_with_tasks(
            "wait-oneshot",
            WaitOwner::Listener,
            vec!["t1".into()],
        );
        let stamp = h.stamp.clone();
        reg.register(h).await.unwrap();
        reg.transfer_owner(
            "wait-oneshot",
            &stamp,
            WaitOwner::ContinuationCoordinator,
        )
        .await
        .unwrap();

        let (tx, rcv) = tokio::sync::oneshot::channel();
        let transferred = TransferredWait::new(
            stamp.clone(),
            vec!["t1".into()],
            rx,
            reg.clone(),
            CancellationToken::new(),
        );
        assert!(
            tx.send(transferred).is_ok(),
            "oneshot must deliver TransferredWait"
        );
        let mut got = rcv.await.expect("receiver must get TransferredWait");
        assert_eq!(got.stamp(), &stamp);
        assert_eq!(got.task_ids(), &["t1".to_string()]);
        assert!(got.cancel_rx().borrow().is_none());
        // Keep registration for cancel proof, then disarm so Drop is clean.
        got.disarm_cleanup();
        assert_eq!(
            reg.owner("wait-oneshot").await,
            Some(WaitOwner::ContinuationCoordinator)
        );
        let _ = reg.deregister(&stamp).await;
    }

    #[tokio::test]
    async fn transfer_oneshot_drop_tx_without_send_aborts_receiver() {
        let (tx, rcv) =
            tokio::sync::oneshot::channel::<TransferredWait>();
        drop(tx); // transfer failed: drop without send
        assert!(
            rcv.await.is_err(),
            "worker must observe failed transfer as oneshot closed"
        );
    }

    #[tokio::test]
    async fn duplicate_task_ids_single_target() {
        // normalize_wait_task_ids: trim, drop empty, de-dupe first-seen order.
        assert_eq!(
            normalize_wait_task_ids(&[
                "  a".into(),
                "".into(),
                "b".into(),
                "a".into(),
                "  ".into(),
                " b ".into(),
                "c".into(),
            ]),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );

        let reg = WaitCancelRegistry::new();
        let (h, _) = handle_with_tasks(
            "wait-dup",
            WaitOwner::Listener,
            // Raw duplicates / whitespace; register must normalize.
            vec![
                " task-dup ".into(),
                "task-dup".into(),
                "".into(),
                "task-other".into(),
                "task-dup".into(),
            ],
        );
        reg.register(h).await.unwrap();

        let targets = reg
            .exact_match_progress_targets("task-dup", "conn-1", "inc-1", 3)
            .await;
        assert_eq!(targets, vec![target("wait-dup", "tool-wait")]);
        // Only one registration entry; membership still exact for other id.
        assert_eq!(
            reg.exact_match_progress_targets("task-other", "conn-1", "inc-1", 3)
                .await,
            vec![target("wait-dup", "tool-wait")]
        );
    }
}
