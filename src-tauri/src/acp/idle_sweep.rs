//! Periodic sweeper that disconnects ACP connections idle past a deadline.
//!
//! Connections accumulate when frontends close their window/tab without
//! triggering an explicit disconnect — common in web mode (browser tab
//! close has no server-side hook), and possible on desktop after panics.
//! The sweep prevents long-lived processes from leaking ACP child
//! processes, file handles, and memory.

use std::time::Duration;

use crate::acp::manager::ConnectionManager;

/// Default shared-root idle threshold (15 minutes). Override at startup via
/// `CODEG_ACP_IDLE_TIMEOUT_SECS`. The sweep only runs against
/// connections in `Connected` state with no `pending_permission`, and
/// `last_activity_at` is bumped on every emit and on every frontend
/// keepalive touch (~30s cadence for open tabs), so an actively-used
/// or visible connection never qualifies.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 900;
pub const DEFAULT_CLIENT_LEASE_TTL_SECS: u64 = 90;
/// Sweep cadence — runs once per minute. Each tick is a brief lock on the
/// connections map plus per-state `try_read`s, so a 1-minute interval is
/// trivially cheap relative to the wall-clock idle threshold.
pub const SWEEP_INTERVAL_SECS: u64 = 60;

/// Read the idle timeout from `CODEG_ACP_IDLE_TIMEOUT_SECS`, falling back
/// to `DEFAULT_IDLE_TIMEOUT_SECS`. A `0` value disables Ready-session and
/// legacy idle reclamation while periodic lease/tombstone maintenance keeps
/// running. Any unparseable value is treated as "use default".
pub fn idle_timeout_from_env() -> Option<Duration> {
    let secs = match std::env::var("CODEG_ACP_IDLE_TIMEOUT_SECS") {
        Ok(raw) => raw.parse::<u64>().unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS),
        Err(_) => DEFAULT_IDLE_TIMEOUT_SECS,
    };
    if secs == 0 {
        return None;
    }
    Some(Duration::from_secs(secs))
}

/// Read the shared client-lease TTL and keep it strictly below an enabled
/// shared idle grace. Invalid or unsafe values use the 90-second default.
pub fn client_lease_ttl_from_env(idle_timeout: Option<Duration>) -> Duration {
    let parsed = std::env::var("CODEG_ACP_CLIENT_LEASE_TTL_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_CLIENT_LEASE_TTL_SECS);
    if idle_timeout.is_some_and(|idle| Duration::from_secs(parsed) >= idle) {
        tracing::warn!(
            "[ACP] client lease TTL must be below enabled idle grace; using {} seconds",
            DEFAULT_CLIENT_LEASE_TTL_SECS
        );
        Duration::from_secs(DEFAULT_CLIENT_LEASE_TTL_SECS)
    } else {
        Duration::from_secs(parsed)
    }
}

/// Run one maintenance pass. Lease expiry and failed-tombstone reclamation are
/// unconditional; Ready-session and legacy idle reclamation require an enabled
/// idle grace.
async fn run_idle_sweep_pass(
    manager: &ConnectionManager,
    idle_timeout: Option<Duration>,
    client_lease_ttl: Duration,
) -> (usize, usize) {
    manager
        .shared_session_broker()
        .expire_leases(tokio::time::Instant::now())
        .await;
    let shared = manager
        .sweep_shared_sessions(idle_timeout, client_lease_ttl)
        .await;
    let legacy = match idle_timeout {
        Some(idle_timeout) => manager.sweep_idle(idle_timeout).await,
        None => 0,
    };
    (shared.removed_count, legacy)
}

/// Long-running task that runs shared and legacy maintenance on a fixed
/// interval. The caller spawns the returned future onto whichever
/// runtime they manage (`tokio::spawn` from inside an async context,
/// `tauri::async_runtime::spawn` from a Tauri `setup` callback that runs
/// outside the runtime).
///
/// Never exits on its own — the caller drops the spawned handle when
/// shutting down (process exit cleans up everything).
pub async fn idle_sweep_task(
    manager: ConnectionManager,
    idle_timeout: Option<Duration>,
    interval: Duration,
) {
    let client_lease_ttl = client_lease_ttl_from_env(idle_timeout);
    manager.configure_shared_client_lease_ttl(client_lease_ttl);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First `tick().await` returns immediately. Skip it so we don't
    // sweep at startup before any connections have a chance to settle.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let (shared, legacy) = run_idle_sweep_pass(&manager, idle_timeout, client_lease_ttl).await;
        if shared > 0 || legacy > 0 {
            tracing::info!(
                "[ACP] idle sweep reclaimed {} shared and {} legacy connection(s)",
                shared,
                legacy
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        acp::{
            session_attach::SessionAttachMode,
            shared_session::{
                SharedLaunchIdentity, SharedMutationGuard, SharedReserveRequest, SharedSessionKey,
            },
        },
        auto_title::ConnectionPurpose,
        models::agent::AgentType,
    };

    fn shared_request(conversation_id: i32, connection_id: &str) -> SharedReserveRequest {
        SharedReserveRequest {
            key: SharedSessionKey::Conversation(conversation_id),
            connection_id: connection_id.into(),
            launch_identity: SharedLaunchIdentity {
                agent_type: AgentType::Codex,
                working_dir_fingerprint: "idle-sweep-cwd".into(),
                external_session_id: None,
                attach_mode: SessionAttachMode::Default,
                route_fingerprint: "idle-sweep-route".into(),
                terminal_shell_fingerprint: "idle-sweep-shell".into(),
                purpose: ConnectionPurpose::User,
            },
            client_instance_id: format!("idle-sweep-client-{conversation_id}"),
            device_id: "idle-sweep-device".into(),
            request_id: format!("idle-sweep-request-{conversation_id}"),
            retry_failed_generation: None,
            now: tokio::time::Instant::now(),
            now_utc: chrono::Utc::now(),
        }
    }

    /// Single test sequences all env-var assertions to avoid the
    /// notorious parallel-test race on shared environment state. Cargo runs
    /// tests in parallel by default, so both ACP timer variables stay here.
    #[test]
    fn idle_timeout_env_parsing() {
        // Disabled when zero.
        std::env::set_var("CODEG_ACP_IDLE_TIMEOUT_SECS", "0");
        assert!(idle_timeout_from_env().is_none());

        // Falls back to default when unparseable.
        std::env::set_var("CODEG_ACP_IDLE_TIMEOUT_SECS", "not-a-number");
        assert_eq!(
            idle_timeout_from_env().unwrap().as_secs(),
            DEFAULT_IDLE_TIMEOUT_SECS
        );

        // Uses provided value when it parses.
        std::env::set_var("CODEG_ACP_IDLE_TIMEOUT_SECS", "120");
        assert_eq!(idle_timeout_from_env().unwrap().as_secs(), 120);

        // Falls back to default when unset.
        std::env::remove_var("CODEG_ACP_IDLE_TIMEOUT_SECS");
        assert_eq!(
            idle_timeout_from_env().unwrap().as_secs(),
            DEFAULT_IDLE_TIMEOUT_SECS
        );

        std::env::set_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS", "45");
        assert_eq!(
            client_lease_ttl_from_env(Some(Duration::from_secs(900))).as_secs(),
            45
        );
        std::env::set_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS", "900");
        assert_eq!(
            client_lease_ttl_from_env(Some(Duration::from_secs(900))).as_secs(),
            DEFAULT_CLIENT_LEASE_TTL_SECS
        );
        std::env::set_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS", "invalid");
        assert_eq!(
            client_lease_ttl_from_env(Some(Duration::from_secs(900))).as_secs(),
            DEFAULT_CLIENT_LEASE_TTL_SECS
        );
        std::env::set_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS", "120");
        assert_eq!(client_lease_ttl_from_env(None).as_secs(), 120);
        std::env::set_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS", "0");
        assert_eq!(
            client_lease_ttl_from_env(None).as_secs(),
            DEFAULT_CLIENT_LEASE_TTL_SECS
        );
        std::env::remove_var("CODEG_ACP_CLIENT_LEASE_TTL_SECS");
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_idle_grace_still_expires_leases_and_reaps_failed_tombstones() {
        let manager = ConnectionManager::new();
        let broker = manager.shared_session_broker();

        let failed = broker
            .reserve_or_attach(shared_request(9_001, "disabled-idle-failed"))
            .await
            .unwrap()
            .attachment;
        broker
            .mark_failed(
                &failed.connection_id,
                failed.generation,
                "session_unavailable",
                true,
            )
            .await
            .unwrap();

        let ready = broker
            .reserve_or_attach(shared_request(9_002, "disabled-idle-ready"))
            .await
            .unwrap()
            .attachment;
        manager
            .install_test_shared_connection(&ready, Some(9_002))
            .await
            .unwrap();
        broker
            .mark_ready(&ready.connection_id, ready.generation, "test-driver-1")
            .await
            .unwrap();
        broker
            .release_lease(&SharedMutationGuard {
                connection_id: ready.connection_id.clone(),
                generation: ready.generation,
                lease_id: ready.lease_id,
            })
            .await
            .unwrap();

        assert_eq!(
            run_idle_sweep_pass(&manager, None, Duration::from_secs(90)).await,
            (0, 0)
        );
        tokio::time::advance(Duration::from_secs(90)).await;
        assert_eq!(
            run_idle_sweep_pass(&manager, None, Duration::from_secs(90)).await,
            (0, 0)
        );
        tokio::time::advance(Duration::from_secs(90)).await;
        assert_eq!(
            run_idle_sweep_pass(&manager, None, Duration::from_secs(90)).await,
            (1, 0)
        );
        assert!(broker
            .diagnostic_for_connection(&failed.connection_id)
            .await
            .is_none());

        tokio::time::advance(Duration::from_secs(900)).await;
        assert_eq!(
            run_idle_sweep_pass(&manager, None, Duration::from_secs(90)).await,
            (0, 0)
        );
        assert!(broker
            .diagnostic_for_connection(&ready.connection_id)
            .await
            .is_some());
    }
}
