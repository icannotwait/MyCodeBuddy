//! Tool-execution watchdog settings persistence and lease control APIs.
//!
//! Shared `_core` functions power both Tauri commands and Axum handlers so
//! desktop/server stay on one clamp + persist + live-apply path.
//!
//! Persisted `app_metadata` keys (no schema migration):
//! - `tool_watchdog.enabled`
//! - `tool_watchdog.warning_after_seconds`
//! - `tool_watchdog.grace_seconds`

use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde::{Deserialize, Serialize};

use crate::acp::manager::ConnectionManager;
use crate::acp::tool_watchdog::{
    StaleLease, ToolWatchdogProjection, ToolWatchdogSettings, MIN_DURATION_SECS, MAX_DURATION_SECS,
};
use crate::app_error::{AppCommandError, AppErrorCode};
use crate::db::service::app_metadata_service;

pub const KEY_TOOL_WATCHDOG_ENABLED: &str = "tool_watchdog.enabled";
pub const KEY_TOOL_WATCHDOG_WARNING_AFTER: &str = "tool_watchdog.warning_after_seconds";
pub const KEY_TOOL_WATCHDOG_GRACE: &str = "tool_watchdog.grace_seconds";

/// Stable CAS/stale failure code (matches [`StaleLease`] display).
pub const STALE_TOOL_WATCHDOG_LEASE: &str = "stale_tool_watchdog_lease";

fn stale_error() -> AppCommandError {
    AppCommandError::new(AppErrorCode::InvalidInput, STALE_TOOL_WATCHDOG_LEASE)
}

fn map_stale(err: StaleLease) -> AppCommandError {
    let _ = err;
    stale_error()
}

/// Read + clamp settings. Missing or malformed values use product defaults
/// (enabled=true, 600/600); out-of-range durations clamp to 60..=3600.
///
/// All three keys are loaded in one query so concurrent saves cannot yield a
/// mixed pre-/post-commit snapshot.
pub async fn load_tool_watchdog_settings_from<C: ConnectionTrait>(
    conn: &C,
) -> Result<ToolWatchdogSettings, crate::db::error::DbError> {
    let values = app_metadata_service::get_values_conn(
        conn,
        &[
            KEY_TOOL_WATCHDOG_ENABLED,
            KEY_TOOL_WATCHDOG_WARNING_AFTER,
            KEY_TOOL_WATCHDOG_GRACE,
        ],
    )
    .await?;
    let mut settings = ToolWatchdogSettings::default();
    if let Some(raw) = values.get(KEY_TOOL_WATCHDOG_ENABLED) {
        if let Ok(v) = raw.parse::<bool>() {
            settings.enabled = v;
        }
    }
    if let Some(raw) = values.get(KEY_TOOL_WATCHDOG_WARNING_AFTER) {
        if let Ok(v) = raw.parse::<u32>() {
            settings.warning_after_seconds = v;
        }
    }
    if let Some(raw) = values.get(KEY_TOOL_WATCHDOG_GRACE) {
        if let Ok(v) = raw.parse::<u32>() {
            settings.grace_seconds = v;
        }
    }
    Ok(settings.clamp())
}

/// Soft load: DB failures resolve to defaults so settings UI never hard-fails.
pub async fn load_tool_watchdog_settings(conn: &DatabaseConnection) -> ToolWatchdogSettings {
    load_tool_watchdog_settings_from(conn)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "failed to load tool watchdog settings; using defaults"
            );
            ToolWatchdogSettings::default()
        })
}

async fn persist_settings_keys<C: ConnectionTrait>(
    conn: &C,
    settings: &ToolWatchdogSettings,
) -> Result<(), AppCommandError> {
    app_metadata_service::upsert_value(
        conn,
        KEY_TOOL_WATCHDOG_ENABLED,
        &settings.enabled.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_TOOL_WATCHDOG_WARNING_AFTER,
        &settings.warning_after_seconds.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_TOOL_WATCHDOG_GRACE,
        &settings.grace_seconds.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    Ok(())
}

/// Load clamped settings from `app_metadata` and apply to the live registry.
/// Call once at desktop/server startup **before** spawning the supervisor.
/// Startup always begins with an empty in-memory registry; old `in_progress`
/// rows are reconciled by existing boot paths, not by replaying leases here.
pub async fn apply_persisted_tool_watchdog_settings(
    conn: &DatabaseConnection,
    manager: &ConnectionManager,
) {
    let settings = load_tool_watchdog_settings(conn).await;
    let cleared = manager
        .tool_lease_registry()
        .apply_settings(settings)
        .await;
    // Startup registry is empty; still emit if a warm re-apply demotes leases.
    manager.emit_tool_watchdog_clears(cleared).await;
    manager.wake_tool_watchdog();
}

/// Shared core: return current persisted (clamped) settings.
pub async fn acp_get_tool_watchdog_settings_core(
    conn: &DatabaseConnection,
) -> ToolWatchdogSettings {
    load_tool_watchdog_settings(conn).await
}

/// Shared core: clamp → persist transaction → live apply to registry only after
/// a successful commit. Failed persistence never mutates the live registry.
///
/// Write + apply are serialized on the manager's settings gate so concurrent
/// saves cannot apply an older commit after a newer one.
pub async fn acp_set_tool_watchdog_settings_core(
    conn: &DatabaseConnection,
    manager: &ConnectionManager,
    desired: ToolWatchdogSettings,
) -> Result<ToolWatchdogSettings, AppCommandError> {
    let clamped = desired.clamp();
    // Guard: clamp must keep durations in product range.
    debug_assert!((MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&clamped.warning_after_seconds));
    debug_assert!((MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&clamped.grace_seconds));

    let settings_gate = manager.tool_watchdog_settings_gate();
    let _settings_guard = settings_gate.lock().await;

    let txn = conn
        .begin()
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;
    persist_settings_keys(&txn, &clamped).await?;
    txn.commit()
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;

    // Commit succeeded — only now update the live registry.
    let cleared = manager
        .tool_lease_registry()
        .apply_settings(clamped)
        .await;
    // Disable demotes Warning/Grace → Running; publish Cleared so attach maps
    // drop stale Stop/Extend surfaces for every affected connection.
    manager.emit_tool_watchdog_clears(cleared).await;
    manager.wake_tool_watchdog();
    Ok(clamped)
}

/// Shared core: CAS extend. Stale version/phase returns `stale_tool_watchdog_lease`.
pub async fn acp_tool_watchdog_extend_core(
    manager: &ConnectionManager,
    lease_id: String,
    version: u64,
) -> Result<ToolWatchdogProjection, AppCommandError> {
    manager
        .tool_watchdog_extend(&lease_id, version)
        .await
        .map_err(map_stale)
}

/// Shared core: CAS user stop. Stale version/phase returns `stale_tool_watchdog_lease`.
pub async fn acp_tool_watchdog_cancel_core(
    manager: &ConnectionManager,
    lease_id: String,
    version: u64,
) -> Result<ToolWatchdogProjection, AppCommandError> {
    manager
        .tool_watchdog_user_cancel(&lease_id, version)
        .await
        .map_err(map_stale)
}

// ─── Tauri commands ─────────────────────────────────────────────────────────

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn acp_get_tool_watchdog_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
) -> Result<ToolWatchdogSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        Ok(acp_get_tool_watchdog_settings_core(&db.conn).await)
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn acp_set_tool_watchdog_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
    #[cfg(feature = "tauri-runtime")] manager: tauri::State<'_, ConnectionManager>,
    enabled: bool,
    warning_after_seconds: u32,
    grace_seconds: u32,
) -> Result<ToolWatchdogSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        acp_set_tool_watchdog_settings_core(
            &db.conn,
            manager.inner(),
            ToolWatchdogSettings {
                enabled,
                warning_after_seconds,
                grace_seconds,
            },
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (enabled, warning_after_seconds, grace_seconds);
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn acp_tool_watchdog_extend(
    #[cfg(feature = "tauri-runtime")] manager: tauri::State<'_, ConnectionManager>,
    lease_id: String,
    version: u64,
) -> Result<ToolWatchdogProjection, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        acp_tool_watchdog_extend_core(manager.inner(), lease_id, version).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (lease_id, version);
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn acp_tool_watchdog_cancel(
    #[cfg(feature = "tauri-runtime")] manager: tauri::State<'_, ConnectionManager>,
    lease_id: String,
    version: u64,
) -> Result<ToolWatchdogProjection, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        acp_tool_watchdog_cancel_core(manager.inner(), lease_id, version).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (lease_id, version);
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

/// Wire-body shape for extend/cancel (Tauri named args / Axum JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolWatchdogLeaseAction {
    pub lease_id: String,
    pub version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::tool_watchdog::{
        RegisterTool, ToolCategory, ToolWatchdogPhase, TurnStamp, WatchdogInstant,
    };
    use crate::db::test_helpers::fresh_in_memory_db;
    use serde_json::json;

    async fn setup() -> (crate::db::AppDatabase, ConnectionManager) {
        let db = fresh_in_memory_db().await;
        let manager = ConnectionManager::new();
        assert_eq!(
            manager.tool_lease_registry().live_lease_count().await,
            0,
            "startup registry must begin empty"
        );
        (db, manager)
    }

    #[tokio::test]
    async fn missing_and_malformed_settings_use_defaults_and_clamp() {
        let (db, manager) = setup().await;

        let defaults = acp_get_tool_watchdog_settings_core(&db.conn).await;
        assert!(defaults.enabled);
        assert_eq!(defaults.warning_after_seconds, 600);
        assert_eq!(defaults.grace_seconds, 600);

        // Malformed → defaults
        app_metadata_service::upsert_value(&db.conn, KEY_TOOL_WATCHDOG_ENABLED, "not-a-bool")
            .await
            .unwrap();
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_TOOL_WATCHDOG_WARNING_AFTER,
            "nope",
        )
        .await
        .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_TOOL_WATCHDOG_GRACE, "")
            .await
            .unwrap();
        let soft = load_tool_watchdog_settings(&db.conn).await;
        assert!(soft.enabled);
        assert_eq!(soft.warning_after_seconds, 600);
        assert_eq!(soft.grace_seconds, 600);

        // Out of range: 59 → 60, 3601 → 3600
        let set = acp_set_tool_watchdog_settings_core(
            &db.conn,
            &manager,
            ToolWatchdogSettings {
                enabled: false,
                warning_after_seconds: 59,
                grace_seconds: 3_601,
            },
        )
        .await
        .expect("set");
        assert!(!set.enabled);
        assert_eq!(set.warning_after_seconds, 60);
        assert_eq!(set.grace_seconds, 3_600);

        let live = manager.tool_lease_registry().settings().await;
        assert_eq!(live, set);

        let reloaded = load_tool_watchdog_settings(&db.conn).await;
        assert_eq!(reloaded, set);

        // Persisted strings are the clamped values.
        let raw_w = app_metadata_service::get_value(&db.conn, KEY_TOOL_WATCHDOG_WARNING_AFTER)
            .await
            .unwrap()
            .unwrap();
        let raw_g = app_metadata_service::get_value(&db.conn, KEY_TOOL_WATCHDOG_GRACE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raw_w, "60");
        assert_eq!(raw_g, "3600");
    }

    #[tokio::test]
    async fn settings_update_live_registry_only_after_successful_persist() {
        let (db, manager) = setup().await;

        // Baseline defaults on live registry.
        assert!(manager.tool_lease_registry().settings().await.enabled);

        // Successful set disables and applies live.
        let set = acp_set_tool_watchdog_settings_core(
            &db.conn,
            &manager,
            ToolWatchdogSettings {
                enabled: false,
                warning_after_seconds: 120,
                grace_seconds: 90,
            },
        )
        .await
        .unwrap();
        assert!(!set.enabled);
        assert_eq!(set.warning_after_seconds, 120);
        assert_eq!(set.grace_seconds, 90);
        let live = manager.tool_lease_registry().settings().await;
        assert!(!live.enabled);
        assert_eq!(live.warning_after_seconds, 120);
        assert_eq!(live.grace_seconds, 90);

        // apply_persisted at startup path reloads into registry.
        manager
            .tool_lease_registry()
            .apply_settings(ToolWatchdogSettings::default())
            .await;
        assert!(manager.tool_lease_registry().settings().await.enabled);
        apply_persisted_tool_watchdog_settings(&db.conn, &manager).await;
        let after = manager.tool_lease_registry().settings().await;
        assert!(!after.enabled);
        assert_eq!(after.warning_after_seconds, 120);
    }

    /// Failed persistence must leave the live registry on its prior settings.
    #[tokio::test]
    async fn failed_persist_leaves_live_settings_unchanged() {
        let (db, manager) = setup().await;
        let established = acp_set_tool_watchdog_settings_core(
            &db.conn,
            &manager,
            ToolWatchdogSettings {
                enabled: false,
                warning_after_seconds: 180,
                grace_seconds: 240,
            },
        )
        .await
        .expect("baseline set");
        let live_before = manager.tool_lease_registry().settings().await;
        assert_eq!(live_before, established);

        // Bare in-memory DB with no migrations → upsert fails (no app_metadata table).
        let bare = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("bare sqlite");
        let err = acp_set_tool_watchdog_settings_core(
            &bare,
            &manager,
            ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 999,
                grace_seconds: 999,
            },
        )
        .await
        .expect_err("persist must fail without schema");
        let _ = err;

        let live_after = manager.tool_lease_registry().settings().await;
        assert_eq!(
            live_after, live_before,
            "failed persist must not mutate live registry"
        );
        // Durable state on the real DB is unchanged.
        let durable = load_tool_watchdog_settings(&db.conn).await;
        assert_eq!(durable, live_before);
    }

    /// Concurrent saves serialize write+apply; final live matches durable.
    #[tokio::test]
    async fn concurrent_settings_saves_keep_live_and_durable_aligned() {
        let (db, manager) = setup().await;
        let a = ToolWatchdogSettings {
            enabled: false,
            warning_after_seconds: 100,
            grace_seconds: 110,
        };
        let b = ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 200,
            grace_seconds: 210,
        };
        let m1 = manager.clone_ref();
        let m2 = manager.clone_ref();
        let conn1 = db.conn.clone();
        let conn2 = db.conn.clone();
        let (r1, r2) = tokio::join!(
            acp_set_tool_watchdog_settings_core(&conn1, &m1, a),
            acp_set_tool_watchdog_settings_core(&conn2, &m2, b),
        );
        let set_a = r1.expect("set a");
        let set_b = r2.expect("set b");
        assert_eq!(set_a, a.clamp());
        assert_eq!(set_b, b.clamp());

        let live = manager.tool_lease_registry().settings().await;
        let durable = load_tool_watchdog_settings(&db.conn).await;
        assert_eq!(
            live, durable,
            "serialized write+apply must leave live == durable"
        );
        assert!(
            live == a.clamp() || live == b.clamp(),
            "final settings must be one of the concurrent writers: {live:?}"
        );
    }

    async fn grace_lease(manager: &ConnectionManager) -> (String, u64) {
        let reg = manager.tool_lease_registry();
        reg.apply_settings(ToolWatchdogSettings {
            enabled: true,
            warning_after_seconds: 60,
            grace_seconds: 60,
        })
        .await;
        let at = WatchdogInstant::now();
        let turn = TurnStamp {
            connection_id: "conn-tw".into(),
            connection_incarnation: "inc-1".into(),
            session_id: "sess".into(),
            turn_generation: 1,
        };
        let _stamp = reg
            .register_tool(RegisterTool {
                turn,
                tool_call_id: "tool-1".into(),
                category: ToolCategory::Terminal,
                at,
            })
            .await
            .unwrap();
        // Advance past warning threshold.
        let warn_at = at.advanced(60);
        let actions = reg.scan(warn_at).await;
        let warn = actions
            .into_iter()
            .find_map(|a| match a {
                crate::acp::tool_watchdog::RegistryAction::PublishWarning { stamp, .. } => {
                    Some(stamp)
                }
                _ => None,
            })
            .expect("warning");
        let grace = reg
            .warning_published(&warn.lease_id, warn.version, warn_at)
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);
        (grace.lease_id, grace.version)
    }

    #[tokio::test]
    async fn extend_and_cancel_reject_stale_without_mutation() {
        let (_db, manager) = setup().await;
        let (lease_id, version) = grace_lease(&manager).await;

        // Stale version on extend.
        let err = acp_tool_watchdog_extend_core(&manager, lease_id.clone(), version + 99)
            .await
            .expect_err("stale extend");
        assert_eq!(err.message, STALE_TOOL_WATCHDOG_LEASE);
        let still = manager
            .tool_lease_registry()
            .lease_stamp(&lease_id)
            .await
            .expect("lease still live");
        assert_eq!(still.version, version);

        // Stale cancel.
        let err = acp_tool_watchdog_cancel_core(&manager, lease_id.clone(), version + 1)
            .await
            .expect_err("stale cancel");
        assert_eq!(err.message, STALE_TOOL_WATCHDOG_LEASE);
        let still = manager
            .tool_lease_registry()
            .lease_stamp(&lease_id)
            .await
            .expect("lease still live");
        assert_eq!(still.version, version);

        // Good extend bumps version.
        let extended = acp_tool_watchdog_extend_core(&manager, lease_id.clone(), version)
            .await
            .expect("extend");
        assert_eq!(extended.phase, ToolWatchdogPhase::Grace);
        assert_eq!(extended.version, version + 1);

        // Cancel with previous version is stale.
        let err = acp_tool_watchdog_cancel_core(&manager, lease_id.clone(), version)
            .await
            .expect_err("stale after extend");
        assert_eq!(err.message, STALE_TOOL_WATCHDOG_LEASE);

        // Current version cancels.
        let cancelled =
            acp_tool_watchdog_cancel_core(&manager, lease_id.clone(), extended.version)
                .await
                .expect("cancel");
        assert_eq!(cancelled.phase, ToolWatchdogPhase::Cancelling);

        // Second cancel is stale (already Cancelling).
        let err = acp_tool_watchdog_cancel_core(&manager, lease_id, cancelled.version)
            .await
            .expect_err("second cancel stale");
        assert_eq!(err.message, STALE_TOOL_WATCHDOG_LEASE);
    }

    #[test]
    fn lease_action_body_is_only_lease_id_and_version() {
        let body = ToolWatchdogLeaseAction {
            lease_id: "lease-1".into(),
            version: 3,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            json!({
                "lease_id": "lease-1",
                "version": 3,
            })
        );
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        for forbidden in [
            "tool_call_id",
            "connection_id",
            "cancel_token",
            "session_id",
            "terminal_id",
            "raw_input",
        ] {
            assert!(!obj.contains_key(forbidden));
        }
    }

    #[tokio::test]
    async fn metrics_extension_and_user_stop_are_secret_safe() {
        let (_db, manager) = setup().await;
        let (lease_id, version) = grace_lease(&manager).await;
        let before = manager.tool_watchdog_metrics().snapshot();
        let _ = acp_tool_watchdog_extend_core(&manager, lease_id.clone(), version)
            .await
            .unwrap();
        let mid = manager.tool_watchdog_metrics().snapshot();
        assert_eq!(mid.extensions_total, before.extensions_total + 1);

        let stamp = manager
            .tool_lease_registry()
            .lease_stamp(&lease_id)
            .await
            .unwrap();
        let _ = acp_tool_watchdog_cancel_core(&manager, lease_id, stamp.version)
            .await
            .unwrap();
        let after = manager.tool_watchdog_metrics().snapshot();
        assert_eq!(after.user_stops_total, mid.user_stops_total + 1);

        let text = serde_json::to_string(&after).unwrap();
        for forbidden in [
            "tool_call_id",
            "raw_input",
            "cancel_token",
            "session_id",
            "terminal_id",
            "api_key",
            "Bearer ",
        ] {
            assert!(
                !text.contains(forbidden),
                "metrics must not contain {forbidden}"
            );
        }
    }
}
