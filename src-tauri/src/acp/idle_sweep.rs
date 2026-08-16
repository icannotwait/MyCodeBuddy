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
/// to `DEFAULT_IDLE_TIMEOUT_SECS`. A `0` value disables the sweep
/// (returns `None`); any unparseable value is treated as "use default".
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

/// Long-running task that calls `ConnectionManager::sweep_idle` on a
/// fixed interval. The caller spawns the returned future onto whichever
/// runtime they manage (`tokio::spawn` from inside an async context,
/// `tauri::async_runtime::spawn` from a Tauri `setup` callback that runs
/// outside the runtime).
///
/// Never exits on its own — the caller drops the spawned handle when
/// shutting down (process exit cleans up everything).
pub async fn idle_sweep_task(
    manager: ConnectionManager,
    idle_timeout: Duration,
    interval: Duration,
) {
    let client_lease_ttl = client_lease_ttl_from_env(Some(idle_timeout));
    manager.configure_shared_client_lease_ttl(client_lease_ttl);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First `tick().await` returns immediately. Skip it so we don't
    // sweep at startup before any connections have a chance to settle.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        manager
            .shared_session_broker()
            .expire_leases(tokio::time::Instant::now())
            .await;
        let shared = manager
            .sweep_shared_sessions(idle_timeout, client_lease_ttl)
            .await;
        let legacy = manager.sweep_idle(idle_timeout).await;
        if shared.removed_count > 0 || legacy > 0 {
            tracing::info!(
                "[ACP] idle sweep reclaimed {} shared and {} legacy connection(s)",
                shared.removed_count,
                legacy
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
