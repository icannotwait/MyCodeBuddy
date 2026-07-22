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

/// Host-only registry of parked multi-task wait cancel handles.
#[derive(Debug, Default)]
pub struct WaitCancelRegistry {
    inner: Mutex<HashMap<String, RegisteredWait>>,
}

#[derive(Debug)]
struct RegisteredWait {
    stamp: WaitStamp,
    owner: WaitOwner,
    /// `None` until cancelled; then carries the initiating cause so the
    /// waiter can emit `tool_stalled_timeout` vs `user_cancelled`.
    cancel: tokio::sync::watch::Sender<Option<CancelCause>>,
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
pub struct WaitCancelGuard {
    registry: Arc<WaitCancelRegistry>,
    stamp: Option<WaitStamp>,
}

impl WaitCancelGuard {
    pub fn new(registry: Arc<WaitCancelRegistry>, stamp: WaitStamp) -> Self {
        Self {
            registry,
            stamp: Some(stamp),
        }
    }

    /// Stop Drop from re-deregistering after an explicit cleanup path.
    pub fn disarm(&mut self) {
        self.stamp = None;
    }
}

impl Drop for WaitCancelGuard {
    fn drop(&mut self) {
        let Some(stamp) = self.stamp.take() else {
            return;
        };
        let registry = self.registry.clone();
        // Async-safe cleanup: peer-close abandons `process_status` without an
        // explicit deregister await. Spawn on the current runtime when present.
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
        let (tx, rx) = new_wait_cancel_channel();
        (
            WaitCancelHandle {
                stamp: stamp(wait_id),
                owner,
                cancel: tx,
            },
            rx,
        )
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
}
