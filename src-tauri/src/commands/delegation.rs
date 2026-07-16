//! Delegation settings persistence + Tauri/HTTP command surface.
//!
//! These knobs survive across restarts:
//!   * `delegation.enabled` — feature kill switch (default false)
//!   * `delegation.depth_limit` — max chain depth a child is allowed to sit at
//!   * `delegation.route_policy` — global managed route default (`codeg`/`native`)
//!   * `delegation.stalled_after_seconds` — soft-watchdog threshold (observe only)
//!   * `delegation.agent_defaults` — per-agent spawn overrides (JSON blob)
//!   * `delegation.completed_cache_max_mb` — per-parent byte budget (in MB) for
//!     the broker's in-memory cache of completed result text (`0` = unlimited)
//!
//! On startup `apply_persisted_config` reads these keys from `app_metadata`
//! and pushes broker-owned fields into the live `DelegationBroker` while
//! pushing route/watchdog/enabled into the shared [`DelegationRuntimeSettings`]
//! watch channel (from one clamped load). On UI save, `set_delegation_settings_core`
//! writes keys in a transaction and only then updates broker + runtime —
//! a failed commit never notifies the watch channel. Route policy is intentionally
//! **not** part of `DelegationConfig`; route resolution consumes the runtime
//! snapshot. The previously-persisted `delegation.default_timeout_seconds` key
//! is ignored on read (the broker no longer applies a timeout; cancellation
//! flows through MCP `notifications/cancelled` instead).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
#[cfg(any(test, feature = "tauri-runtime"))]
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::acp::delegation::broker::{DelegationBroker, DelegationConfig};
use crate::acp::delegation::route::DelegationRoutePolicy;
use crate::acp::delegation::types::{
    AgentDelegationDefaults, DelegationProfile, DelegationProfileDocument,
};
use crate::app_error::AppCommandError;
use crate::db::service::app_metadata_service;
use crate::models::AgentType;

pub const KEY_DELEGATION_ENABLED: &str = "delegation.enabled";
pub const KEY_DELEGATION_DEPTH: &str = "delegation.depth_limit";
/// Single JSON-serialized key for the per-agent delegation overrides.
/// Stored as one blob (rather than one row per agent×option) because the
/// option set is dynamic and per-agent — flat keys can't enumerate it.
pub const KEY_DELEGATION_AGENT_DEFAULTS: &str = "delegation.agent_defaults";
/// Per-parent completed-result cache budget, in MB. `0` = unlimited.
pub const KEY_DELEGATION_COMPLETED_CACHE_MB: &str = "delegation.completed_cache_max_mb";
pub const KEY_DELEGATION_PROFILES_V1: &str = "delegation.profiles.v1";
/// Global managed-route default (`codeg` or `native`).
pub const KEY_DELEGATION_ROUTE_POLICY: &str = "delegation.route_policy";
/// Soft-watchdog stall threshold in seconds (observe-only consumers).
pub const KEY_DELEGATION_STALLED_AFTER_SECONDS: &str = "delegation.stalled_after_seconds";

pub const DEPTH_MIN: u32 = 1;
pub const DEPTH_MAX: u32 = 8;

/// Product default for the completed-result cache budget, in MB. Used by
/// `DelegationSettings::default()` and as the serde fallback when a payload
/// omits the field (absent ≠ unlimited).
pub const DEFAULT_COMPLETED_CACHE_MB: u32 = 512;

/// Product default for the soft-watchdog stall threshold (seconds).
pub const DEFAULT_STALLED_AFTER_SECONDS: u32 = 300;
pub const STALLED_AFTER_MIN: u32 = 60;
pub const STALLED_AFTER_MAX: u32 = 3600;

fn default_completed_cache_max_mb() -> u32 {
    DEFAULT_COMPLETED_CACHE_MB
}

fn default_route_policy() -> DelegationRoutePolicy {
    DelegationRoutePolicy::Codeg
}

fn default_stalled_after_seconds() -> u32 {
    DEFAULT_STALLED_AFTER_SECONDS
}

/// Newtype so the Tauri managed-state lookup can distinguish the delegation
/// UDS path from other `PathBuf`s in the state graph.
#[derive(Clone)]
pub struct DelegationSocketPath(pub PathBuf);

/// Live subset of delegation settings consumed by route resolution and the
/// soft-watchdog supervisor. Updated only after a successful DB commit (or a
/// single clamped load at startup) so consumers never see a half-applied write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationRuntimeSnapshot {
    pub enabled: bool,
    pub route_policy: DelegationRoutePolicy,
    pub stalled_after_seconds: u32,
}

impl Default for DelegationRuntimeSnapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            route_policy: default_route_policy(),
            stalled_after_seconds: DEFAULT_STALLED_AFTER_SECONDS,
        }
    }
}

/// Shared watch-backed handle for [`DelegationRuntimeSnapshot`]. Cloned into
/// `AppState` and managed Tauri state so desktop/server/test paths share one
/// live value. Route resolution and the soft watchdog subscribe here; Broker
/// keeps only creation/profile/cache settings in `DelegationConfig`.
#[derive(Clone)]
pub struct DelegationRuntimeSettings {
    tx: tokio::sync::watch::Sender<DelegationRuntimeSnapshot>,
}

impl Default for DelegationRuntimeSettings {
    fn default() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(DelegationRuntimeSnapshot::default());
        Self { tx }
    }
}

impl DelegationRuntimeSettings {
    pub fn snapshot(&self) -> DelegationRuntimeSnapshot {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<DelegationRuntimeSnapshot> {
        self.tx.subscribe()
    }

    pub fn set(&self, snapshot: DelegationRuntimeSnapshot) {
        // `send` returns Err and drops the value when the channel has zero
        // receivers. `send_replace` always retains the latest snapshot so
        // startup `apply_persisted_config` works before any subscriber attaches.
        self.tx.send_replace(snapshot);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationSettings {
    pub enabled: bool,
    pub depth_limit: u32,
    /// Global managed-route default. Absent in a legacy payload → `codeg`.
    #[serde(default = "default_route_policy")]
    pub route_policy: DelegationRoutePolicy,
    /// Soft-watchdog stall threshold (seconds). Absent in a legacy payload →
    /// product default; clamped to `STALLED_AFTER_MIN..=STALLED_AFTER_MAX`.
    #[serde(default = "default_stalled_after_seconds")]
    pub stalled_after_seconds: u32,
    /// Per-agent default overrides applied by the delegation broker when
    /// codeg-mcp spawns a subagent. Empty map → no overrides anywhere,
    /// which is the pre-existing behavior.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_defaults: BTreeMap<AgentType, AgentDelegationDefaults>,
    /// Per-parent byte budget (in MB) for the broker's in-memory cache of
    /// completed sub-agent result text. `0` = unlimited. Converted to bytes in
    /// `into_broker_config`. Absent in a payload → the product default (not
    /// unlimited), so an older client can't silently disable the valve.
    #[serde(default = "default_completed_cache_max_mb")]
    pub completed_cache_max_mb: u32,
}

impl Default for DelegationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            depth_limit: 1,
            route_policy: default_route_policy(),
            stalled_after_seconds: DEFAULT_STALLED_AFTER_SECONDS,
            agent_defaults: BTreeMap::new(),
            completed_cache_max_mb: DEFAULT_COMPLETED_CACHE_MB,
        }
    }
}

impl DelegationSettings {
    fn clamped(self) -> Self {
        Self {
            enabled: self.enabled,
            depth_limit: self.depth_limit.clamp(DEPTH_MIN, DEPTH_MAX),
            route_policy: self.route_policy,
            stalled_after_seconds: self
                .stalled_after_seconds
                .clamp(STALLED_AFTER_MIN, STALLED_AFTER_MAX),
            agent_defaults: self
                .agent_defaults
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .collect(),
            // No upper clamp: the cache budget is a user memory choice, not a
            // safety rail. `0` stays `0` (unlimited).
            completed_cache_max_mb: self.completed_cache_max_mb,
        }
    }

    fn into_broker_config(self) -> DelegationConfig {
        // Intentionally omits `route_policy` / `stalled_after_seconds` — those
        // live on `DelegationRuntimeSettings`, not `DelegationConfig`.
        DelegationConfig {
            enabled: self.enabled,
            depth_limit: self.depth_limit,
            agent_defaults: self.agent_defaults,
            profiles: BTreeMap::new(),
            // MB → bytes. `saturating_mul` guards a pathologically large MB
            // value from wrapping on 32-bit `usize` targets.
            completed_cache_cap_bytes: (self.completed_cache_max_mb as usize)
                .saturating_mul(1024 * 1024),
        }
    }

    fn into_runtime_snapshot(&self) -> DelegationRuntimeSnapshot {
        DelegationRuntimeSnapshot {
            enabled: self.enabled,
            route_policy: self.route_policy,
            stalled_after_seconds: self.stalled_after_seconds,
        }
    }
}

fn parse_route_policy(raw: &str) -> DelegationRoutePolicy {
    // Exact wire values; anything else (including empty) falls back to Codeg.
    match raw.trim() {
        "native" => DelegationRoutePolicy::Native,
        "codeg" => DelegationRoutePolicy::Codeg,
        other => serde_json::from_str::<DelegationRoutePolicy>(&format!("\"{other}\""))
            .unwrap_or(DelegationRoutePolicy::Codeg),
    }
}

fn route_policy_to_storage(policy: DelegationRoutePolicy) -> &'static str {
    match policy {
        DelegationRoutePolicy::Codeg => "codeg",
        DelegationRoutePolicy::Native => "native",
    }
}

/// Read all persisted keys from `app_metadata`, falling back to defaults
/// for any missing or malformed value. Never errors hard — corrupt
/// persistence is treated as "no preference yet."
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
    if let Ok(Some(raw)) = app_metadata_service::get_value(conn, KEY_DELEGATION_ROUTE_POLICY).await
    {
        // Malformed route strings fall back to Codeg (not a parse-then-clamp).
        settings.route_policy = parse_route_policy(&raw);
    }
    if let Ok(Some(raw)) =
        app_metadata_service::get_value(conn, KEY_DELEGATION_STALLED_AFTER_SECONDS).await
    {
        // Numeric parse first; non-numeric keeps product default (300). Out-of
        // range values are clamped below, not rejected.
        if let Ok(v) = raw.parse::<u32>() {
            settings.stalled_after_seconds = v;
        }
    }
    if let Ok(Some(raw)) =
        app_metadata_service::get_value(conn, KEY_DELEGATION_COMPLETED_CACHE_MB).await
    {
        if let Ok(v) = raw.parse::<u32>() {
            settings.completed_cache_max_mb = v;
        }
    }
    if let Ok(Some(raw)) =
        app_metadata_service::get_value(conn, KEY_DELEGATION_AGENT_DEFAULTS).await
    {
        // Corrupt JSON → keep defaults (empty map). Matches the "never errors
        // hard" contract on the other two keys above.
        if let Ok(parsed) =
            serde_json::from_str::<BTreeMap<AgentType, AgentDelegationDefaults>>(&raw)
        {
            settings.agent_defaults = parsed;
        }
    }
    settings.clamped()
}

/// Pull settings from the DB and push Broker config + the runtime watch
/// snapshot from **one** loaded/clamped `DelegationSettings` value so startup
/// cannot expose mismatched route/watchdog snapshots. Idempotent — safe to
/// call on startup, after settings save, or after any external write to
/// `app_metadata`.
///
/// Profile load failures do **not** wipe a healthy live profile map: we keep
/// whatever the broker currently holds and log. Corrupt DB rows still fail
/// hard on explicit `get_delegation_profiles`.
pub async fn apply_persisted_config(
    conn: &DatabaseConnection,
    broker: &DelegationBroker,
    runtime: &DelegationRuntimeSettings,
) {
    let settings = load_delegation_settings(conn).await;
    runtime.set(settings.into_runtime_snapshot());
    let mut config = settings.into_broker_config();
    // Preserve currently-live profiles unless a replacement loads cleanly.
    config.profiles = broker.config_snapshot().await.profiles;
    match load_delegation_profiles(conn).await {
        Ok(document) => {
            config.profiles = document
                .profiles
                .into_iter()
                .map(|profile| (profile.id.clone(), profile))
                .collect();
        }
        Err(error) => {
            eprintln!("[Delegation] failed to load profiles; keeping live map: {error}");
        }
    }
    broker.set_config(config).await;
}

async fn persist_settings_keys<C: sea_orm::ConnectionTrait>(
    conn: &C,
    clamped: &DelegationSettings,
) -> Result<(), AppCommandError> {
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
        KEY_DELEGATION_ROUTE_POLICY,
        route_policy_to_storage(clamped.route_policy),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_DELEGATION_STALLED_AFTER_SECONDS,
        &clamped.stalled_after_seconds.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(
        conn,
        KEY_DELEGATION_COMPLETED_CACHE_MB,
        &clamped.completed_cache_max_mb.to_string(),
    )
    .await
    .map_err(AppCommandError::from)?;
    // Whole-blob replace semantics: save mirrors what the UI sent. Empty map
    // serializes to "{}" — still write it so a user can clear all overrides
    // back to the agent defaults.
    let agent_defaults_json = serde_json::to_string(&clamped.agent_defaults).map_err(|e| {
        AppCommandError::configuration_invalid(format!("serialize agent_defaults: {e}"))
    })?;
    app_metadata_service::upsert_value(conn, KEY_DELEGATION_AGENT_DEFAULTS, &agent_defaults_json)
        .await
        .map_err(AppCommandError::from)?;
    Ok(())
}

/// Persist + apply. Used by both the Tauri command and the HTTP handler so
/// the clamp / re-apply chain is in exactly one place. Settings keys are
/// written in one DB transaction so a mid-write failure does not leave a
/// partial settings document. The runtime watch channel is updated **only
/// after** a successful commit. Route/enabled changes refresh managed-root
/// route staleness; a watchdog-only save updates the channel without stale.
pub async fn set_delegation_settings_core(
    conn: &DatabaseConnection,
    broker: &DelegationBroker,
    runtime: &DelegationRuntimeSettings,
    manager: &crate::acp::manager::ConnectionManager,
    desired: DelegationSettings,
) -> Result<DelegationSettings, AppCommandError> {
    use sea_orm::TransactionTrait;
    let before = runtime.snapshot();
    let clamped = desired.clamped();
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
    // Commit succeeded — notify live consumers. Must not run on txn failure.
    let after = clamped.into_runtime_snapshot();
    runtime.set(after.clone());
    let profiles = broker.config_snapshot().await.profiles;
    let mut config = clamped.clone().into_broker_config();
    config.profiles = profiles;
    broker.set_config(config).await;
    if before.enabled != after.enabled || before.route_policy != after.route_policy {
        manager
            .refresh_delegation_route_staleness(after.route_policy, after.enabled)
            .await;
    }
    Ok(clamped)
}

/// Combined settings + profiles document saved in one DB transaction, then
/// applied to the broker in a single `set_config` so concurrent delegations
/// never observe "new settings + old profiles". Runtime watch is updated only
/// after the transaction commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationBundle {
    pub settings: DelegationSettings,
    pub profiles: DelegationProfileDocument,
}

pub async fn set_delegation_bundle_core(
    conn: &DatabaseConnection,
    broker: &DelegationBroker,
    runtime: &DelegationRuntimeSettings,
    manager: &crate::acp::manager::ConnectionManager,
    desired: DelegationBundle,
) -> Result<DelegationBundle, AppCommandError> {
    use sea_orm::TransactionTrait;
    let before = runtime.snapshot();
    let clamped = desired.settings.clamped();
    let normalized = DelegationProfileDocument {
        profiles: normalize_profiles(desired.profiles.profiles)?,
    };
    let profiles_json = serde_json::to_string(&normalized).map_err(|e| {
        AppCommandError::configuration_invalid(format!("serialize delegation profiles: {e}"))
    })?;
    let txn = conn
        .begin()
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;
    persist_settings_keys(&txn, &clamped).await?;
    app_metadata_service::upsert_value(&txn, KEY_DELEGATION_PROFILES_V1, &profiles_json)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;

    let after = clamped.into_runtime_snapshot();
    runtime.set(after.clone());
    let mut config = clamped.clone().into_broker_config();
    config.profiles = normalized
        .profiles
        .iter()
        .cloned()
        .map(|profile| (profile.id.clone(), profile))
        .collect();
    broker.set_config(config).await;
    if before.enabled != after.enabled || before.route_policy != after.route_policy {
        manager
            .refresh_delegation_route_staleness(after.route_policy, after.enabled)
            .await;
    }
    Ok(DelegationBundle {
        settings: clamped,
        profiles: normalized,
    })
}

fn normalize_profiles(
    profiles: Vec<DelegationProfile>,
) -> Result<Vec<DelegationProfile>, AppCommandError> {
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut normalized = Vec::with_capacity(profiles.len());
    for mut profile in profiles {
        if uuid::Uuid::parse_str(&profile.id).is_err() {
            return Err(AppCommandError::configuration_invalid(format!(
                "invalid delegation profile id: {}",
                profile.id
            )));
        }
        if !ids.insert(profile.id.clone()) {
            return Err(AppCommandError::configuration_invalid(format!(
                "duplicate delegation profile id: {}",
                profile.id
            )));
        }
        profile.name = profile.name.trim().to_string();
        if profile.name.is_empty() || profile.name.chars().count() > 80 {
            return Err(AppCommandError::configuration_invalid(
                "delegation profile name must contain 1-80 characters",
            ));
        }
        let name_key = (profile.agent_type, profile.name.to_lowercase());
        if !names.insert(name_key) {
            return Err(AppCommandError::configuration_invalid(format!(
                "duplicate profile name for {}: {}",
                profile.agent_type, profile.name
            )));
        }
        normalized.push(profile);
    }
    Ok(normalized)
}

pub async fn load_delegation_profiles(
    conn: &DatabaseConnection,
) -> Result<DelegationProfileDocument, AppCommandError> {
    let Some(raw) = app_metadata_service::get_value(conn, KEY_DELEGATION_PROFILES_V1)
        .await
        .map_err(AppCommandError::from)?
    else {
        return Ok(DelegationProfileDocument::default());
    };
    let document: DelegationProfileDocument = serde_json::from_str(&raw).map_err(|e| {
        AppCommandError::configuration_invalid(format!("parse delegation profiles: {e}"))
    })?;
    Ok(DelegationProfileDocument {
        profiles: normalize_profiles(document.profiles)?,
    })
}

pub async fn set_delegation_profiles_core(
    conn: &DatabaseConnection,
    desired: DelegationProfileDocument,
) -> Result<DelegationProfileDocument, AppCommandError> {
    let normalized = DelegationProfileDocument {
        profiles: normalize_profiles(desired.profiles)?,
    };
    let json = serde_json::to_string(&normalized).map_err(|e| {
        AppCommandError::configuration_invalid(format!("serialize delegation profiles: {e}"))
    })?;
    app_metadata_service::upsert_value(conn, KEY_DELEGATION_PROFILES_V1, &json)
        .await
        .map_err(AppCommandError::from)?;
    Ok(normalized)
}

/// Apply profiles to the broker after a successful profiles-only persist.
/// Kept separate so the web/Tauri commands share the same sequence:
/// DB write first, then live map (best-effort consistency on process death).
pub async fn apply_profiles_to_broker(
    broker: &DelegationBroker,
    document: &DelegationProfileDocument,
) {
    broker
        .set_profiles(
            document
                .profiles
                .iter()
                .cloned()
                .map(|profile| (profile.id.clone(), profile))
                .collect(),
        )
        .await;
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
    #[cfg(feature = "tauri-runtime")] runtime: tauri::State<'_, DelegationRuntimeSettings>,
    #[cfg(feature = "tauri-runtime")] manager: tauri::State<
        '_,
        crate::acp::manager::ConnectionManager,
    >,
    settings: DelegationSettings,
) -> Result<DelegationSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        set_delegation_settings_core(
            &db.conn,
            broker.inner(),
            runtime.inner(),
            manager.inner(),
            settings,
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = settings;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_delegation_profiles(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
) -> Result<DelegationProfileDocument, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        load_delegation_profiles(&db.conn).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_delegation_profiles(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
    #[cfg(feature = "tauri-runtime")] broker: tauri::State<'_, Arc<DelegationBroker>>,
    document: DelegationProfileDocument,
) -> Result<DelegationProfileDocument, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let saved = set_delegation_profiles_core(&db.conn, document).await?;
        apply_profiles_to_broker(broker.inner(), &saved).await;
        Ok(saved)
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = document;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_delegation_bundle(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
    #[cfg(feature = "tauri-runtime")] broker: tauri::State<'_, Arc<DelegationBroker>>,
    #[cfg(feature = "tauri-runtime")] runtime: tauri::State<'_, DelegationRuntimeSettings>,
    #[cfg(feature = "tauri-runtime")] manager: tauri::State<
        '_,
        crate::acp::manager::ConnectionManager,
    >,
    bundle: DelegationBundle,
) -> Result<DelegationBundle, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        set_delegation_bundle_core(
            &db.conn,
            broker.inner(),
            runtime.inner(),
            manager.inner(),
            bundle,
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = bundle;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

/// Snapshot process-local delegation reliability metrics (debug / operator use).
/// Shared by the authenticated HTTP debug handler; no product UI consumer.
pub fn get_delegation_metrics_core(
    metrics: &crate::acp::delegation::metrics::DelegationMetrics,
) -> crate::acp::delegation::metrics::DelegationMetricsSnapshot {
    metrics.snapshot()
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

    fn profile(id: &str, name: &str) -> DelegationProfile {
        DelegationProfile {
            id: id.into(),
            agent_type: AgentType::CodeBuddy,
            name: name.into(),
            mode_id: Some("default".into()),
            config_values: BTreeMap::from([("model".into(), "glm-5.2".into())]),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn profiles_trim_names_and_reject_case_folded_duplicates() {
        let profiles = vec![
            profile("11111111-1111-4111-8111-111111111111", " GLM5.2 "),
            profile("22222222-2222-4222-8222-222222222222", "glm5.2"),
        ];
        let err = normalize_profiles(profiles).unwrap_err();
        assert!(err.to_string().contains("duplicate profile name"));
    }

    #[test]
    fn profile_name_limit_counts_unicode_scalars() {
        let mut p = profile("11111111-1111-4111-8111-111111111111", &"模".repeat(81));
        assert!(normalize_profiles(vec![p.clone()]).is_err());
        p.name = "模".repeat(80);
        assert_eq!(
            normalize_profiles(vec![p]).unwrap()[0].name.chars().count(),
            80
        );
    }

    #[tokio::test]
    async fn profiles_round_trip_and_corrupt_json_is_not_silently_empty() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let document = DelegationProfileDocument {
            profiles: vec![profile("11111111-1111-4111-8111-111111111111", " GLM5.2 ")],
        };
        let saved = set_delegation_profiles_core(&db.conn, document)
            .await
            .unwrap();
        assert_eq!(saved.profiles[0].name, "GLM5.2");
        assert_eq!(load_delegation_profiles(&db.conn).await.unwrap(), saved);

        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_PROFILES_V1, "{")
            .await
            .unwrap();
        assert!(load_delegation_profiles(&db.conn).await.is_err());
    }

    #[tokio::test]
    async fn apply_persisted_config_keeps_live_profiles_when_db_corrupt() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let live = profile("11111111-1111-4111-8111-111111111111", "Live");
        broker
            .set_profiles(BTreeMap::from([(live.id.clone(), live.clone())]))
            .await;
        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_PROFILES_V1, "{")
            .await
            .unwrap();
        apply_persisted_config(&db.conn, &broker, &runtime).await;
        let cfg = broker.config_snapshot().await;
        assert_eq!(
            cfg.profiles.get(&live.id).map(|p| p.name.as_str()),
            Some("Live")
        );
    }

    #[tokio::test]
    async fn bundle_save_writes_settings_and_profiles_atomically() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let bundle = DelegationBundle {
            settings: DelegationSettings {
                enabled: true,
                depth_limit: 3,
                ..DelegationSettings::default()
            },
            profiles: DelegationProfileDocument {
                profiles: vec![profile("11111111-1111-4111-8111-111111111111", " GLM5.2 ")],
            },
        };
        let saved = set_delegation_bundle_core(&db.conn, &broker, &runtime, &crate::acp::manager::ConnectionManager::new(), bundle)
            .await
            .unwrap();
        assert!(saved.settings.enabled);
        assert_eq!(saved.settings.depth_limit, 3);
        assert_eq!(saved.profiles.profiles[0].name, "GLM5.2");
        assert_eq!(load_delegation_settings(&db.conn).await.depth_limit, 3);
        assert_eq!(
            load_delegation_profiles(&db.conn).await.unwrap().profiles[0].name,
            "GLM5.2"
        );
        let cfg = broker.config_snapshot().await;
        assert!(cfg.enabled);
        assert_eq!(cfg.depth_limit, 3);
        assert_eq!(
            cfg.profiles
                .get("11111111-1111-4111-8111-111111111111")
                .map(|p| p.name.as_str()),
            Some("GLM5.2")
        );
    }

    #[test]
    fn settings_clamp_to_safe_range() {
        let s = DelegationSettings {
            enabled: true,
            depth_limit: 99,
            ..DelegationSettings::default()
        }
        .clamped();
        assert_eq!(s.depth_limit, DEPTH_MAX);
    }

    #[tokio::test]
    async fn load_returns_defaults_when_unset() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = load_delegation_settings(&db.conn).await;
        assert!(!settings.enabled);
        assert_eq!(settings.depth_limit, 1);
    }

    #[tokio::test]
    async fn set_then_load_round_trip_and_broker_applied() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let desired = DelegationSettings {
            enabled: false,
            depth_limit: 3,
            ..DelegationSettings::default()
        };
        let saved = set_delegation_settings_core(&db.conn, &broker, &runtime, &crate::acp::manager::ConnectionManager::new(), desired)
            .await
            .unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.depth_limit, 3);

        let loaded = load_delegation_settings(&db.conn).await;
        assert_eq!(loaded.enabled, saved.enabled);
        assert_eq!(loaded.depth_limit, saved.depth_limit);

        let cfg = broker.config_snapshot().await;
        assert!(!cfg.enabled);
        assert_eq!(cfg.depth_limit, 3);
    }

    #[tokio::test]
    async fn agent_defaults_round_trip_through_db_and_broker() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();

        let mut claude_cfg = BTreeMap::new();
        claude_cfg.insert("model".into(), "claude-sonnet-4-5".into());
        let mut agent_defaults: BTreeMap<AgentType, AgentDelegationDefaults> = BTreeMap::new();
        agent_defaults.insert(
            AgentType::ClaudeCode,
            AgentDelegationDefaults {
                mode_id: Some("auto".into()),
                config_values: claude_cfg.clone(),
            },
        );

        let desired = DelegationSettings {
            enabled: true,
            depth_limit: 4,
            agent_defaults: agent_defaults.clone(),
            ..DelegationSettings::default()
        };
        let saved = set_delegation_settings_core(&db.conn, &broker, &runtime, &crate::acp::manager::ConnectionManager::new(), desired)
            .await
            .unwrap();
        assert_eq!(saved.agent_defaults, agent_defaults);

        // Re-read from DB — the JSON blob should round-trip identically.
        let loaded = load_delegation_settings(&db.conn).await;
        assert_eq!(loaded.agent_defaults, agent_defaults);

        // Broker should have the same map applied.
        let cfg = broker.config_snapshot().await;
        let entry = cfg.agent_defaults.get(&AgentType::ClaudeCode).unwrap();
        assert_eq!(entry.mode_id.as_deref(), Some("auto"));
        assert_eq!(entry.config_values, claude_cfg);
    }

    #[tokio::test]
    async fn clamped_drops_empty_agent_defaults_entries() {
        // Empty entries (no mode, no config_values) should be filtered out so
        // the persisted JSON stays compact.
        let mut agent_defaults: BTreeMap<AgentType, AgentDelegationDefaults> = BTreeMap::new();
        agent_defaults.insert(AgentType::ClaudeCode, AgentDelegationDefaults::default());
        agent_defaults.insert(
            AgentType::Codex,
            AgentDelegationDefaults {
                mode_id: Some("auto".into()),
                config_values: BTreeMap::new(),
            },
        );
        let s = DelegationSettings {
            enabled: true,
            depth_limit: 2,
            agent_defaults,
            ..DelegationSettings::default()
        }
        .clamped();
        assert!(!s.agent_defaults.contains_key(&AgentType::ClaudeCode));
        assert!(s.agent_defaults.contains_key(&AgentType::Codex));
    }

    #[tokio::test]
    async fn set_clamps_out_of_range_values() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let saved = set_delegation_settings_core(
            &db.conn,
            &broker,
            &runtime,
            &crate::acp::manager::ConnectionManager::new(),
            DelegationSettings {
                enabled: true,
                depth_limit: 999,
                ..DelegationSettings::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.depth_limit, DEPTH_MAX);
    }

    #[tokio::test]
    async fn completed_cache_mb_round_trips_and_converts_to_bytes() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let desired = DelegationSettings {
            enabled: true,
            depth_limit: 1,
            completed_cache_max_mb: 8,
            ..DelegationSettings::default()
        };
        let saved = set_delegation_settings_core(&db.conn, &broker, &runtime, &crate::acp::manager::ConnectionManager::new(), desired)
            .await
            .unwrap();
        assert_eq!(saved.completed_cache_max_mb, 8);

        // Persisted + reloaded identically.
        let loaded = load_delegation_settings(&db.conn).await;
        assert_eq!(loaded.completed_cache_max_mb, 8);

        // Broker received the MB → bytes conversion.
        let cfg = broker.config_snapshot().await;
        assert_eq!(cfg.completed_cache_cap_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn completed_cache_mb_zero_means_unlimited_and_is_not_clamped() {
        let s = DelegationSettings {
            completed_cache_max_mb: 0,
            ..DelegationSettings::default()
        }
        .clamped();
        assert_eq!(s.completed_cache_max_mb, 0);
        assert_eq!(s.into_broker_config().completed_cache_cap_bytes, 0);
    }

    #[tokio::test]
    async fn load_returns_default_completed_cache_when_unset() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = load_delegation_settings(&db.conn).await;
        assert_eq!(settings.completed_cache_max_mb, DEFAULT_COMPLETED_CACHE_MB);
    }

    #[tokio::test]
    async fn route_and_watchdog_settings_default_parse_and_clamp() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let defaults = load_delegation_settings(&db.conn).await;
        assert_eq!(defaults.route_policy, DelegationRoutePolicy::Codeg);
        assert_eq!(defaults.stalled_after_seconds, 300);

        // Non-numeric watchdog values keep the product default (300) before any
        // numeric under/over clamp cases below.
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_DELEGATION_STALLED_AFTER_SECONDS,
            "not-a-number",
        )
        .await
        .unwrap();
        let non_numeric = load_delegation_settings(&db.conn).await;
        assert_eq!(non_numeric.stalled_after_seconds, 300);

        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_ROUTE_POLICY, "broken")
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_STALLED_AFTER_SECONDS, "9")
            .await
            .unwrap();
        let malformed = load_delegation_settings(&db.conn).await;
        assert_eq!(malformed.route_policy, DelegationRoutePolicy::Codeg);
        assert_eq!(malformed.stalled_after_seconds, 60);

        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_ROUTE_POLICY, "native")
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_DELEGATION_STALLED_AFTER_SECONDS, "9000")
            .await
            .unwrap();
        let persisted = load_delegation_settings(&db.conn).await;
        assert_eq!(persisted.route_policy, DelegationRoutePolicy::Native);
        assert_eq!(persisted.stalled_after_seconds, 3600);
    }

    /// Startup applies persisted settings via `set` before any consumer has
    /// called `subscribe`. With zero receivers, `Sender::send` drops the value
    /// and later subscribers would still see the channel default.
    #[test]
    fn runtime_settings_retain_value_with_zero_subscribers() {
        let runtime = DelegationRuntimeSettings::default();
        // No subscribe() yet — mirrors AppState construction before watchdog /
        // route resolvers attach.
        let desired = DelegationRuntimeSnapshot {
            enabled: true,
            route_policy: DelegationRoutePolicy::Native,
            stalled_after_seconds: 120,
        };
        runtime.set(desired.clone());

        assert_eq!(runtime.snapshot(), desired);
        let rx = runtime.subscribe();
        assert_eq!(*rx.borrow(), desired);
    }

    #[tokio::test]
    async fn settings_save_updates_runtime_watch_channel_after_commit() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let broker = make_broker();
        let runtime = DelegationRuntimeSettings::default();
        let mut rx = runtime.subscribe();
        let desired = DelegationSettings {
            enabled: true,
            depth_limit: 2,
            route_policy: DelegationRoutePolicy::Native,
            stalled_after_seconds: 120,
            agent_defaults: BTreeMap::new(),
            completed_cache_max_mb: 512,
        };

        let saved = set_delegation_settings_core(&db.conn, &broker, &runtime, &crate::acp::manager::ConnectionManager::new(), desired)
            .await
            .unwrap();
        rx.changed().await.unwrap();
        assert_eq!(saved.route_policy, DelegationRoutePolicy::Native);
        assert_eq!(rx.borrow().stalled_after_seconds, 120);
    }

    #[test]
    fn legacy_settings_payload_gets_new_product_defaults() {
        let settings: DelegationSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "depth_limit": 1,
            "completed_cache_max_mb": 512
        }))
        .unwrap();
        assert_eq!(settings.route_policy, DelegationRoutePolicy::Codeg);
        assert_eq!(settings.stalled_after_seconds, 300);
    }
}
