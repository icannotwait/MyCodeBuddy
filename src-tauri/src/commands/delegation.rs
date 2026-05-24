//! Delegation settings persistence + Tauri/HTTP command surface.
//!
//! Three knobs survive across restarts:
//!   * `delegation.enabled` — feature kill switch (default true)
//!   * `delegation.depth_limit` — max chain depth a child is allowed to sit at
//!   * `delegation.default_timeout_seconds` — broker fallback when the LLM
//!     omits `timeout_seconds`
//!
//! On startup `apply_persisted_config` reads all three keys from
//! `app_metadata` and pushes them into the live `DelegationBroker`. On UI
//! save, `set_delegation_settings_core` writes the three keys and
//! immediately re-applies — the broker has no concept of "pending config",
//! it just owns the current `DelegationConfig`.

use std::path::PathBuf;
#[cfg(any(test, feature = "tauri-runtime"))]
use std::sync::Arc;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::acp::delegation::broker::{DelegationBroker, DelegationConfig};
use crate::app_error::AppCommandError;
use crate::db::service::app_metadata_service;

pub const KEY_DELEGATION_ENABLED: &str = "delegation.enabled";
pub const KEY_DELEGATION_DEPTH: &str = "delegation.depth_limit";
pub const KEY_DELEGATION_TIMEOUT: &str = "delegation.default_timeout_seconds";

pub const DEPTH_MIN: u32 = 1;
pub const DEPTH_MAX: u32 = 8;
pub const TIMEOUT_MIN_SECS: u64 = 30;
pub const TIMEOUT_MAX_SECS: u64 = 3600;

/// Newtype so the Tauri managed-state lookup can distinguish the delegation
/// UDS path from other `PathBuf`s in the state graph.
#[derive(Clone)]
pub struct DelegationSocketPath(pub PathBuf);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationSettings {
    pub enabled: bool,
    pub depth_limit: u32,
    pub default_timeout_seconds: u64,
}

impl Default for DelegationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            depth_limit: 2,
            default_timeout_seconds: 600,
        }
    }
}

impl DelegationSettings {
    fn clamped(self) -> Self {
        Self {
            enabled: self.enabled,
            depth_limit: self.depth_limit.clamp(DEPTH_MIN, DEPTH_MAX),
            default_timeout_seconds: self
                .default_timeout_seconds
                .clamp(TIMEOUT_MIN_SECS, TIMEOUT_MAX_SECS),
        }
    }

    fn into_broker_config(self) -> DelegationConfig {
        DelegationConfig {
            enabled: self.enabled,
            depth_limit: self.depth_limit,
            default_timeout: Duration::from_secs(self.default_timeout_seconds),
        }
    }
}

/// Read all three keys from `app_metadata`, falling back to defaults for any
/// missing or malformed value. Never errors hard — corrupt persistence is
/// treated as "no preference yet."
pub async fn load_delegation_settings(conn: &DatabaseConnection) -> DelegationSettings {
    let mut settings = DelegationSettings::default();
    if let Ok(Some(raw)) = app_metadata_service::get_value(conn, KEY_DELEGATION_ENABLED).await {
        if let Ok(v) = raw.parse::<bool>() {
            settings.enabled = v;
        }
    }
    if let Ok(Some(raw)) = app_metadata_service::get_value(conn, KEY_DELEGATION_DEPTH).await {
        if let Ok(v) = raw.parse::<u32>() {
            settings.depth_limit = v;
        }
    }
    if let Ok(Some(raw)) = app_metadata_service::get_value(conn, KEY_DELEGATION_TIMEOUT).await {
        if let Ok(v) = raw.parse::<u64>() {
            settings.default_timeout_seconds = v;
        }
    }
    settings.clamped()
}

/// Pull settings from the DB and push the resulting `DelegationConfig` onto
/// the broker. Idempotent — safe to call on startup, after settings save, or
/// after any external write to `app_metadata`.
pub async fn apply_persisted_config(conn: &DatabaseConnection, broker: &DelegationBroker) {
    let settings = load_delegation_settings(conn).await;
    broker.set_config(settings.into_broker_config()).await;
}

/// Persist + apply. Used by both the Tauri command and the HTTP handler so
/// the clamp / re-apply chain is in exactly one place.
pub async fn set_delegation_settings_core(
    conn: &DatabaseConnection,
    broker: &DelegationBroker,
    desired: DelegationSettings,
) -> Result<DelegationSettings, AppCommandError> {
    let clamped = desired.clamped();
    app_metadata_service::upsert_value(conn, KEY_DELEGATION_ENABLED, &clamped.enabled.to_string())
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_DELEGATION_DEPTH,
        &clamped.depth_limit.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_DELEGATION_TIMEOUT,
        &clamped.default_timeout_seconds.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    broker
        .set_config(clamped.clone().into_broker_config())
        .await;
    Ok(clamped)
}

// -------- Tauri commands -----------------------------------------------------

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_delegation_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
) -> Result<DelegationSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        Ok(load_delegation_settings(&db.conn).await)
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        // Server mode reaches this via the web handler, not this command.
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_delegation_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
    #[cfg(feature = "tauri-runtime")] broker: tauri::State<'_, Arc<DelegationBroker>>,
    settings: DelegationSettings,
) -> Result<DelegationSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        set_delegation_settings_core(&db.conn, broker.inner(), settings).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = settings;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::DelegationError;
    use async_trait::async_trait;

    struct EmptyLookup;
    #[async_trait]
    impl ConversationDepthLookup for EmptyLookup {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    fn make_broker() -> DelegationBroker {
        DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
        )
    }

    #[test]
    fn settings_clamp_to_safe_range() {
        let s = DelegationSettings {
            enabled: true,
            depth_limit: 99,
            default_timeout_seconds: 10,
        }
        .clamped();
        assert_eq!(s.depth_limit, DEPTH_MAX);
        assert_eq!(s.default_timeout_seconds, TIMEOUT_MIN_SECS);
    }

    #[tokio::test]
    async fn load_returns_defaults_when_unset() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = load_delegation_settings(&db.conn).await;
        assert!(settings.enabled);
        assert_eq!(settings.depth_limit, 2);
        assert_eq!(settings.default_timeout_seconds, 600);
    }

    #[tokio::test]
    async fn set_then_load_round_trip_and_broker_applied() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let desired = DelegationSettings {
            enabled: false,
            depth_limit: 3,
            default_timeout_seconds: 120,
        };
        let saved = set_delegation_settings_core(&db.conn, &broker, desired)
            .await
            .unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.depth_limit, 3);
        assert_eq!(saved.default_timeout_seconds, 120);

        let loaded = load_delegation_settings(&db.conn).await;
        assert_eq!(loaded.enabled, saved.enabled);
        assert_eq!(loaded.depth_limit, saved.depth_limit);
        assert_eq!(
            loaded.default_timeout_seconds,
            saved.default_timeout_seconds
        );

        let cfg = broker.config_snapshot().await;
        assert!(!cfg.enabled);
        assert_eq!(cfg.depth_limit, 3);
        assert_eq!(cfg.default_timeout, Duration::from_secs(120));
    }

    #[tokio::test]
    async fn set_clamps_out_of_range_values() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let saved = set_delegation_settings_core(
            &db.conn,
            &broker,
            DelegationSettings {
                enabled: true,
                depth_limit: 999,
                default_timeout_seconds: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.depth_limit, DEPTH_MAX);
        assert_eq!(saved.default_timeout_seconds, TIMEOUT_MIN_SECS);
    }
}
