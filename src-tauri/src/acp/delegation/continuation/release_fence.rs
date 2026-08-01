use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForegroundMcpReleaseOutcome {
    FrameFlushed,
    PeerClosed,
}

pub(crate) struct ForegroundMcpReleaseOwner {
    tx: Option<oneshot::Sender<ForegroundMcpReleaseOutcome>>,
}

pub(crate) struct ForegroundMcpReleaseWaiter {
    rx: oneshot::Receiver<ForegroundMcpReleaseOutcome>,
}

pub(crate) fn foreground_mcp_release_fence(
) -> (ForegroundMcpReleaseOwner, ForegroundMcpReleaseWaiter) {
    let (tx, rx) = oneshot::channel();
    (
        ForegroundMcpReleaseOwner { tx: Some(tx) },
        ForegroundMcpReleaseWaiter { rx },
    )
}

impl ForegroundMcpReleaseOwner {
    pub(crate) fn frame_flushed(mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ForegroundMcpReleaseOutcome::FrameFlushed);
        }
    }
}

impl Drop for ForegroundMcpReleaseOwner {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ForegroundMcpReleaseOutcome::PeerClosed);
        }
    }
}

impl ForegroundMcpReleaseWaiter {
    pub(crate) async fn wait(mut self) -> ForegroundMcpReleaseOutcome {
        self.wait_ref().await
    }

    async fn wait_ref(&mut self) -> ForegroundMcpReleaseOutcome {
        (&mut self.rx)
            .await
            .unwrap_or(ForegroundMcpReleaseOutcome::PeerClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn foreground_mcp_release_fence_reports_frame_flushed() {
        let (owner, waiter) = foreground_mcp_release_fence();
        owner.frame_flushed();
        assert_eq!(
            waiter.wait().await,
            ForegroundMcpReleaseOutcome::FrameFlushed
        );
    }

    #[tokio::test]
    async fn foreground_mcp_release_fence_owner_drop_reports_peer_closed() {
        let (owner, waiter) = foreground_mcp_release_fence();
        drop(owner);
        assert_eq!(waiter.wait().await, ForegroundMcpReleaseOutcome::PeerClosed);
    }

    #[tokio::test]
    async fn foreground_mcp_release_fence_stays_pending_while_owner_is_armed() {
        let (owner, mut waiter) = foreground_mcp_release_fence();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_millis(20), waiter.wait_ref()).await,
            Err(_)
        ));
        owner.frame_flushed();
        assert_eq!(
            waiter.wait().await,
            ForegroundMcpReleaseOutcome::FrameFlushed
        );
    }
}
