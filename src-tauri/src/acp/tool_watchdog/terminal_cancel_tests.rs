//! Integration-style tests for non-blocking CancelTerminal admission.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::acp::connection::{connection_channel, ConnectionControl};
    use crate::acp::tool_watchdog::{TERMINAL_ACK_TIMEOUT, TERMINAL_ADMIT_TIMEOUT};

    #[tokio::test]
    async fn cancel_terminal_admit_acks_without_waiting_for_kill() {
        let (tx, mut rx, _liveness) = connection_channel::<ConnectionControl>(4);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        // Consumer admits immediately (mirrors connection loop handler).
        let consumer = tokio::spawn(async move {
            match rx.recv().await {
                Some(ConnectionControl::CancelTerminal {
                    session_id,
                    terminal_id,
                    reply,
                }) => {
                    assert_eq!(session_id, "sess-1");
                    assert_eq!(terminal_id, "term-1");
                    // Ack only — do not kill on the control path.
                    let _ = reply.send(Ok(()));
                }
                _ => panic!("expected CancelTerminal"),
            }
        });

        let sent = tokio::time::timeout(TERMINAL_ADMIT_TIMEOUT, async {
            tx.send(ConnectionControl::CancelTerminal {
                session_id: "sess-1".into(),
                terminal_id: "term-1".into(),
                reply: reply_tx,
            })
            .await
        })
        .await;
        assert!(sent.is_ok(), "admit send must complete within admit timeout");
        assert!(sent.unwrap().is_ok());

        let ack = tokio::time::timeout(TERMINAL_ACK_TIMEOUT, reply_rx).await;
        assert!(ack.is_ok(), "ack must arrive within ack timeout");
        assert!(ack.unwrap().unwrap().is_ok());
        consumer.await.unwrap();
    }

    #[tokio::test]
    async fn saturated_control_lane_admit_times_out() {
        // Capacity 1: fill the lane so try_send fails.
        let (tx, _rx, _liveness) = connection_channel::<ConnectionControl>(1);
        tx.try_send(ConnectionControl::Cancel)
            .expect("fill control lane");

        let (reply_tx, _reply_rx) =
            tokio::sync::oneshot::channel::<Result<(), crate::acp::terminal_runtime::TerminalRuntimeError>>();
        let msg = ConnectionControl::CancelTerminal {
            session_id: "s".into(),
            terminal_id: "t".into(),
            reply: reply_tx,
        };
        let start = std::time::Instant::now();
        let admit_deadline = tokio::time::Instant::now() + TERMINAL_ADMIT_TIMEOUT;
        let mut pending = Some(msg);
        let mut failed = false;
        while let Some(m) = pending.take() {
            match tx.try_send(m) {
                Ok(()) => break,
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    if tokio::time::Instant::now() >= admit_deadline {
                        failed = true;
                        break;
                    }
                    pending = Some(returned);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed, "saturated lane must fail admit within budget");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "admit timeout must stay bounded"
        );
    }

    #[tokio::test]
    async fn non_acking_loop_fails_ack_timeout() {
        let (tx, mut rx, _liveness) = connection_channel::<ConnectionControl>(4);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        // Consumer receives but never acks (simulates hung loop).
        let consumer = tokio::spawn(async move {
            let _msg = rx.recv().await;
            // Intentionally drop without answering after a long sleep.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        tx.send(ConnectionControl::CancelTerminal {
            session_id: "s".into(),
            terminal_id: "t".into(),
            reply: reply_tx,
        })
        .await
        .unwrap();

        let ack = tokio::time::timeout(TERMINAL_ACK_TIMEOUT, reply_rx).await;
        assert!(ack.is_err(), "non-acking consumer must hit ack timeout");
        consumer.abort();
    }

    #[tokio::test]
    async fn cancel_terminal_is_not_user_turn_cancel() {
        // Structural: CancelTerminal carries session/terminal ids + reply;
        // Cancel has neither. Match exhaustiveness documents the distinction.
        let (tx, mut rx, _) = connection_channel::<ConnectionControl>(2);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.try_send(ConnectionControl::CancelTerminal {
            session_id: "s".into(),
            terminal_id: "t".into(),
            reply: reply_tx,
        })
        .unwrap();
        tx.try_send(ConnectionControl::Cancel).unwrap();

        match rx.try_recv().unwrap() {
            ConnectionControl::CancelTerminal { reply, .. } => {
                let _ = reply.send(Ok(()));
            }
            ConnectionControl::Cancel => panic!("must not treat CancelTerminal as Cancel"),
            _ => panic!("unexpected control"),
        }
        match rx.try_recv().unwrap() {
            ConnectionControl::Cancel => {}
            _ => panic!("expected user Cancel"),
        }
        assert!(reply_rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn hung_detached_kill_does_not_block_ack() {
        // Ack path is independent of kill completion.
        let ack_done = Arc::new(tokio::sync::Notify::new());
        let ack_done2 = ack_done.clone();
        let (tx, mut rx, _) = connection_channel::<ConnectionControl>(2);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            if let Some(ConnectionControl::CancelTerminal { reply, .. }) = rx.recv().await {
                let _ = reply.send(Ok(()));
                ack_done2.notify_one();
                // Simulated hung kill after ack.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });

        tx.send(ConnectionControl::CancelTerminal {
            session_id: "s".into(),
            terminal_id: "t".into(),
            reply: reply_tx,
        })
        .await
        .unwrap();

        tokio::time::timeout(Duration::from_millis(200), ack_done.notified())
            .await
            .expect("ack notify");
        assert!(reply_rx.await.unwrap().is_ok());
    }
}
