//! Host-only MCP request cancel token registry.
//!
//! Tokens are opaque [`McpCancelToken`] values minted by the connection MCP
//! layer. Capability invocation must acknowledge promptly; hanging cancel
//! futures run under a supervisor deadline with escalation.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::types::{LeaseStamp, McpCancelResult, McpCancelToken};

/// Host-owned cancel function for a pending MCP request.
pub type McpCancelFn = Arc<dyn Fn() -> bool + Send + Sync>;

struct McpCancelEntry {
    stamp: LeaseStamp,
    cancel: McpCancelFn,
    settled: bool,
}

/// Registry of opaque MCP cancel tokens bound to a lease stamp.
#[derive(Default)]
pub struct McpCancelRegistry {
    inner: Mutex<HashMap<u64, McpCancelEntry>>,
    next_id: Mutex<u64>,
}

impl std::fmt::Debug for McpCancelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpCancelRegistry").finish_non_exhaustive()
    }
}

impl McpCancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Mint a token and register a cancel callback for the lease stamp.
    pub async fn register(&self, stamp: LeaseStamp, cancel: McpCancelFn) -> McpCancelToken {
        let id = {
            let mut next = self.next_id.lock().await;
            *next = next.saturating_add(1);
            *next
        };
        self.inner.lock().await.insert(
            id,
            McpCancelEntry {
                stamp,
                cancel,
                settled: false,
            },
        );
        McpCancelToken::new(id)
    }

    /// Invoke cancel for a verified stamp+token pair.
    ///
    /// Clones the callback and drops the mutex **before** invoking it so a
    /// blocking cancel never holds the registry lock under the supervisor
    /// deadline.
    pub async fn cancel(&self, stamp: &LeaseStamp, token: McpCancelToken) -> McpCancelResult {
        let cancel_fn = {
            let mut inner = self.inner.lock().await;
            let Some(entry) = inner.get_mut(&token.0) else {
                return McpCancelResult::NotFound;
            };
            if entry.settled {
                return McpCancelResult::AlreadySettled;
            }
            if &entry.stamp != stamp {
                return McpCancelResult::Stale;
            }
            // Mark settled before drop so concurrent cancel sees AlreadySettled.
            entry.settled = true;
            entry.cancel.clone()
        };
        // Callback runs outside the mutex.
        let ok = (cancel_fn)();
        if ok {
            McpCancelResult::Cancelled
        } else {
            // Server ignored cancel; settled so we do not double-invoke.
            // Supervisor escalates when the lease stays live.
            McpCancelResult::Unsupported
        }
    }

    /// Peer-close / normal completion must deregister.
    pub async fn deregister(&self, stamp: &LeaseStamp, token: McpCancelToken) -> McpCancelResult {
        let mut inner = self.inner.lock().await;
        let Some(entry) = inner.get(&token.0) else {
            return McpCancelResult::NotFound;
        };
        if &entry.stamp != stamp {
            return McpCancelResult::Stale;
        }
        let was_settled = entry.settled;
        inner.remove(&token.0);
        if was_settled {
            McpCancelResult::AlreadySettled
        } else {
            McpCancelResult::Cancelled
        }
    }
}

// Extend McpCancelResult with NotFound for registry (types already have Stale etc.)
// types.rs has: Cancelled, AlreadySettled, Unsupported, Stale, TimedOut
// Add NotFound if missing — check types.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn sample_stamp(lease_id: &str) -> LeaseStamp {
        LeaseStamp {
            lease_id: lease_id.into(),
            version: 1,
            connection_id: "c1".into(),
            connection_incarnation: "i1".into(),
            turn_generation: 1,
            tool_call_id: Some("tool-mcp".into()),
        }
    }

    #[tokio::test]
    async fn mcp_cancel_handle_that_settles() {
        let reg = McpCancelRegistry::new();
        let fired = Arc::new(AtomicBool::new(false));
        let fired2 = fired.clone();
        let stamp = sample_stamp("lease-1");
        let token = reg
            .register(
                stamp.clone(),
                Arc::new(move || {
                    fired2.store(true, Ordering::SeqCst);
                    true
                }),
            )
            .await;

        assert_eq!(
            reg.cancel(&stamp, token).await,
            McpCancelResult::Cancelled
        );
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(
            reg.cancel(&stamp, token).await,
            McpCancelResult::AlreadySettled
        );
    }

    #[tokio::test]
    async fn mcp_that_ignores_cancellation() {
        let reg = McpCancelRegistry::new();
        let stamp = sample_stamp("lease-2");
        let token = reg
            .register(stamp.clone(), Arc::new(|| false))
            .await;
        assert_eq!(
            reg.cancel(&stamp, token).await,
            McpCancelResult::Unsupported
        );
    }

    #[tokio::test]
    async fn deregister_on_settle_then_cancel_not_found() {
        let reg = McpCancelRegistry::new();
        let stamp = sample_stamp("lease-3");
        let token = reg
            .register(stamp.clone(), Arc::new(|| true))
            .await;
        assert_eq!(
            reg.deregister(&stamp, token).await,
            McpCancelResult::Cancelled
        );
        assert_eq!(reg.cancel(&stamp, token).await, McpCancelResult::NotFound);
    }

    #[tokio::test]
    async fn stale_stamp_rejected() {
        let reg = McpCancelRegistry::new();
        let stamp = sample_stamp("lease-4");
        let token = reg
            .register(stamp.clone(), Arc::new(|| true))
            .await;
        let other = sample_stamp("lease-other");
        assert_eq!(reg.cancel(&other, token).await, McpCancelResult::Stale);
    }

    #[tokio::test]
    async fn cancel_does_not_hold_mutex_during_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let reg = Arc::new(McpCancelRegistry::new());
        let stamp = sample_stamp("lease-5");
        let reg_for_cb = reg.clone();
        let reentered = Arc::new(AtomicBool::new(false));
        let reentered2 = reentered.clone();
        // If cancel held the mutex across the callback, try_lock would fail.
        let token = reg
            .register(
                stamp.clone(),
                Arc::new(move || {
                    // Synchronous re-entry probe: lock must be free.
                    if let Ok(guard) = reg_for_cb.inner.try_lock() {
                        reentered2.store(true, Ordering::SeqCst);
                        drop(guard);
                    }
                    true
                }),
            )
            .await;
        assert_eq!(reg.cancel(&stamp, token).await, McpCancelResult::Cancelled);
        assert!(
            reentered.load(Ordering::SeqCst),
            "cancel callback must run without holding the registry mutex"
        );
    }
}
