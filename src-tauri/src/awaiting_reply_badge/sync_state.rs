//! Async badge apply state machine.
//!
//! Serializes full `COUNT → compare → render → apply → cache` behind a mutex.
//! Cache (`last_successfully_applied`) updates only after a successful apply.

use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadgeApplyError {
    /// Main webview window missing — silent at schedule boundary.
    MissingMainWindow,
    Setter(String),
    /// Constructed only from Windows+tauri `apply_overlay_on_main` / schedule path.
    #[cfg_attr(
        not(all(feature = "tauri-runtime", target_os = "windows")),
        allow(dead_code)
    )]
    Enqueue(String),
    /// Constructed only when COUNT fails on Windows desktop schedule path.
    #[cfg_attr(
        not(all(feature = "tauri-runtime", target_os = "windows")),
        allow(dead_code)
    )]
    Count(String),
    /// oneshot receiver dropped / closed before result (Windows apply path).
    #[cfg_attr(
        not(all(feature = "tauri-runtime", target_os = "windows")),
        allow(dead_code)
    )]
    ApplyChannelClosed,
}

/// Process-level apply coordinator. `u32::MAX` = never successfully applied.
pub struct BadgeApplyState {
    lock: Mutex<()>,
    last_successfully_applied: AtomicU32,
}

impl BadgeApplyState {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            last_successfully_applied: AtomicU32::new(u32::MAX),
        }
    }

    /// Last count that completed apply successfully (`u32::MAX` if never).
    /// Public for unit tests; production reads only the atomic under the mutex path.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn last_applied(&self) -> u32 {
        self.last_successfully_applied.load(Ordering::Acquire)
    }

    /// Holds mutex for entire sequence. Awaits `apply` to completion before
    /// updating cache. Cache updates only on `Ok(())` from `apply`.
    pub async fn sync_with_count<CF, FutC, AF, FutA>(
        &self,
        count_fn: CF,
        mut apply: AF,
        render: fn(u32) -> (Vec<u8>, u32, u32),
    ) -> Result<(), BadgeApplyError>
    where
        CF: FnOnce() -> FutC,
        FutC: std::future::Future<Output = Result<u32, BadgeApplyError>>,
        AF: FnMut(Option<(Vec<u8>, u32, u32)>) -> FutA,
        FutA: std::future::Future<Output = Result<(), BadgeApplyError>>,
    {
        let _guard = self.lock.lock().await;
        let count = count_fn().await?;
        let last = self.last_successfully_applied.load(Ordering::Acquire);
        if last == count {
            return Ok(());
        }
        let icon = if count == 0 {
            None
        } else {
            Some(render(count))
        };
        apply(icon).await?;
        self.last_successfully_applied
            .store(count, Ordering::Release);
        Ok(())
    }
}

impl Default for BadgeApplyState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use tokio::sync::{oneshot, Mutex as TokioMutex};

    fn dummy_render(count: u32) -> (Vec<u8>, u32, u32) {
        (vec![count as u8], 1, 1)
    }

    #[tokio::test]
    async fn cache_updates_only_after_successful_apply() {
        let state = BadgeApplyState::new();
        assert_eq!(state.last_applied(), u32::MAX);

        let err = state
            .sync_with_count(
                || async { Ok(5u32) },
                |_icon| async { Err(BadgeApplyError::Setter("boom".into())) },
                dummy_render,
            )
            .await;
        assert_eq!(err, Err(BadgeApplyError::Setter("boom".into())));
        assert_eq!(
            state.last_applied(),
            u32::MAX,
            "failed apply must not update cache"
        );

        state
            .sync_with_count(
                || async { Ok(5u32) },
                |_icon| async { Ok(()) },
                dummy_render,
            )
            .await
            .expect("second apply ok");
        assert_eq!(state.last_applied(), 5);
    }

    #[tokio::test]
    async fn missing_window_leaves_cache_and_allows_retry() {
        let state = BadgeApplyState::new();
        assert_eq!(state.last_applied(), u32::MAX);

        let err = state
            .sync_with_count(
                || async { Ok(2u32) },
                |_icon| async { Err(BadgeApplyError::MissingMainWindow) },
                dummy_render,
            )
            .await;
        assert_eq!(err, Err(BadgeApplyError::MissingMainWindow));
        assert_eq!(state.last_applied(), u32::MAX);

        state
            .sync_with_count(
                || async { Ok(2u32) },
                |_icon| async { Ok(()) },
                dummy_render,
            )
            .await
            .expect("retry after window exists");
        assert_eq!(state.last_applied(), 2);
    }

    #[tokio::test]
    async fn overlapping_syncs_serialize_and_converge() {
        let state = Arc::new(BadgeApplyState::new());

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(TokioMutex::new(Some(release_rx)));
        // oneshot avoids Notify's missed-signal race if park happens before wait.
        let (parked_tx, parked_rx) = oneshot::channel::<()>();
        let parked_tx = Arc::new(TokioMutex::new(Some(parked_tx)));
        let apply_calls = Arc::new(AtomicU32::new(0));

        // Build independent FnMut appliers (each owns Arc clones) so they can
        // move into 'static spawn tasks without borrowing the test stack.
        let build_apply = || {
            let release_rx = Arc::clone(&release_rx);
            let parked_tx = Arc::clone(&parked_tx);
            let apply_calls = Arc::clone(&apply_calls);
            move |_icon: Option<(Vec<u8>, u32, u32)>| {
                let release_rx = Arc::clone(&release_rx);
                let parked_tx = Arc::clone(&parked_tx);
                let apply_calls = Arc::clone(&apply_calls);
                async move {
                    let n = apply_calls.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        // Signal that the first apply is parked under the mutex.
                        if let Some(tx) = parked_tx.lock().await.take() {
                            let _ = tx.send(());
                        }
                        let rx = release_rx
                            .lock()
                            .await
                            .take()
                            .expect("first apply owns release receiver");
                        let _ = rx.await;
                    }
                    Ok::<(), BadgeApplyError>(())
                }
            }
        };

        let apply_a = build_apply();
        let apply_b = build_apply();

        let state_a = Arc::clone(&state);
        let a = tokio::spawn(async move {
            state_a
                .sync_with_count(|| async { Ok(3u32) }, apply_a, dummy_render)
                .await
        });

        // Wait until A's apply is parked (mutex held by A).
        parked_rx.await.expect("A apply parked");

        let state_b = Arc::clone(&state);
        let b = tokio::spawn(async move {
            state_b
                .sync_with_count(|| async { Ok(1u32) }, apply_b, dummy_render)
                .await
        });

        // Give B a chance to block on the mutex while A still holds it.
        tokio::task::yield_now().await;

        release_tx.send(()).expect("release A apply");

        a.await
            .expect("join A")
            .expect("A apply ok → stores count 3");
        b.await
            .expect("join B")
            .expect("B apply ok → stores count 1");

        assert_eq!(
            state.last_applied(),
            1,
            "serialized runs must converge to latest COUNT"
        );
        assert_eq!(apply_calls.load(Ordering::SeqCst), 2);
    }
}
