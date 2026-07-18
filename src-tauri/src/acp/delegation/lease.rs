//! Authenticated companion ready lease.
//!
//! Two-frame handshake: companion sends `BrokerMessage::Ready { token }`,
//! listener authenticates and acks, both keep the socket open. Peer EOF or
//! token revoke marks the lease closed exactly once. Post-ready close flips
//! only `delegation_available` — never the immutable route plan.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};

/// Waiter returned by [`CompanionLeaseRegistry::register`].
#[derive(Debug)]
pub struct CompanionLeaseWaiter {
    ready_rx: watch::Receiver<bool>,
    availability_rx: watch::Receiver<bool>,
}

impl CompanionLeaseWaiter {
    /// Block until the companion authenticates and the listener marks ready,
    /// or until `timeout` elapses.
    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), ReadyLeaseError> {
        if *self.ready_rx.borrow() {
            return Ok(());
        }
        match tokio::time::timeout(timeout, self.ready_rx.changed()).await {
            Ok(Ok(())) if *self.ready_rx.borrow() => Ok(()),
            Ok(Ok(())) => Err(ReadyLeaseError::NotReady),
            Ok(Err(_)) => Err(ReadyLeaseError::Closed),
            Err(_) => Err(ReadyLeaseError::Timeout),
        }
    }

    /// Live availability watch: `true` after ready, `false` after close/revoke.
    pub fn availability(&self) -> watch::Receiver<bool> {
        self.availability_rx.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadyLeaseError {
    #[error("companion ready lease timed out")]
    Timeout,
    #[error("companion ready lease closed before ready")]
    Closed,
    #[error("companion ready lease not ready")]
    NotReady,
    #[error("unknown lease token")]
    UnknownToken,
    #[error("lease already ready")]
    AlreadyReady,
}

struct LeaseSlot {
    ready_tx: watch::Sender<bool>,
    availability_tx: watch::Sender<bool>,
    /// `true` once mark_closed has run (exactly once).
    closed: Arc<std::sync::atomic::AtomicBool>,
}

/// In-process registry of per-token companion ready leases.
#[derive(Default)]
pub struct CompanionLeaseRegistry {
    inner: Mutex<HashMap<String, LeaseSlot>>,
}

impl CompanionLeaseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a lease waiter for `token` before MCP injection exposes the
    /// companion entry. Replaces any prior entry for the same token.
    pub async fn register(&self, token: impl Into<String>) -> CompanionLeaseWaiter {
        let token = token.into();
        let (ready_tx, ready_rx) = watch::channel(false);
        let (availability_tx, availability_rx) = watch::channel(false);
        let waiter = CompanionLeaseWaiter {
            ready_rx,
            availability_rx,
        };
        self.inner.lock().await.insert(
            token,
            LeaseSlot {
                ready_tx,
                availability_tx,
                closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        );
        waiter
    }

    /// Subscribe to availability for a token already registered (listener path).
    pub async fn subscribe_availability(&self, token: &str) -> Option<watch::Receiver<bool>> {
        self.inner
            .lock()
            .await
            .get(token)
            .map(|s| s.availability_tx.subscribe())
    }

    /// Mark the token ready after authentication. Idempotent only for the
    /// first success; a second call returns [`ReadyLeaseError::AlreadyReady`].
    pub async fn mark_ready(&self, token: &str) -> Result<(), ReadyLeaseError> {
        let guard = self.inner.lock().await;
        let slot = guard.get(token).ok_or(ReadyLeaseError::UnknownToken)?;
        if *slot.ready_tx.borrow() {
            return Err(ReadyLeaseError::AlreadyReady);
        }
        let _ = slot.ready_tx.send(true);
        let _ = slot.availability_tx.send(true);
        Ok(())
    }

    /// Mark the lease closed exactly once (peer EOF or revoke). Subsequent
    /// calls are no-ops. Flips availability to `false`.
    pub async fn mark_closed(&self, token: &str) {
        let slot = {
            let guard = self.inner.lock().await;
            match guard.get(token) {
                Some(s) => Some((s.availability_tx.clone(), Arc::clone(&s.closed))),
                None => None,
            }
        };
        let Some((availability_tx, closed)) = slot else {
            return;
        };
        if closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _ = availability_tx.send(false);
    }

    /// Revoke a token: mark closed and drop the slot so further mark_ready fails.
    /// Every token revoke closes its waiter.
    pub async fn revoke(&self, token: &str) {
        self.mark_closed(token).await;
        self.inner.lock().await.remove(token);
    }
}

/// Bounded ready-wait timeout for Codeg routes that expose delegation.
/// Overridable in tests via [`set_ready_lease_timeout_for_test`].
pub fn ready_lease_timeout() -> Duration {
    #[cfg(any(test, feature = "test-utils"))]
    {
        if let Some(d) = TEST_READY_LEASE_TIMEOUT.lock().unwrap().as_ref() {
            return *d;
        }
    }
    Duration::from_secs(30)
}

#[cfg(any(test, feature = "test-utils"))]
static TEST_READY_LEASE_TIMEOUT: std::sync::Mutex<Option<Duration>> = std::sync::Mutex::new(None);

/// Override the ready-lease timeout for deterministic tests. Pass `None` to
/// restore the production default.
#[cfg(any(test, feature = "test-utils"))]
pub fn set_ready_lease_timeout_for_test(d: Option<Duration>) {
    *TEST_READY_LEASE_TIMEOUT.lock().unwrap() = d;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ready_lease_marks_ready_then_unavailable_on_close() {
        let registry = CompanionLeaseRegistry::default();
        let mut waiter = registry.register("tok-1").await;
        registry.mark_ready("tok-1").await.unwrap();
        waiter.wait_ready(Duration::from_millis(50)).await.unwrap();
        assert!(waiter.availability().borrow().to_owned());

        registry.mark_closed("tok-1").await;
        waiter.availability().changed().await.unwrap();
        assert!(!*waiter.availability().borrow());
    }

    #[tokio::test]
    async fn mark_closed_is_idempotent() {
        let registry = CompanionLeaseRegistry::default();
        let waiter = registry.register("tok-2").await;
        registry.mark_ready("tok-2").await.unwrap();
        registry.mark_closed("tok-2").await;
        registry.mark_closed("tok-2").await;
        assert!(!*waiter.availability().borrow());
    }

    #[tokio::test]
    async fn revoke_closes_waiter_and_forgets_token() {
        let registry = CompanionLeaseRegistry::default();
        let mut waiter = registry.register("tok-3").await;
        registry.mark_ready("tok-3").await.unwrap();
        waiter.wait_ready(Duration::from_millis(50)).await.unwrap();
        registry.revoke("tok-3").await;
        // Availability must flip false.
        assert!(!*waiter.availability().borrow());
        assert!(matches!(
            registry.mark_ready("tok-3").await,
            Err(ReadyLeaseError::UnknownToken)
        ));
    }
}
