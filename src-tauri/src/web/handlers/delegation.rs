//! HTTP handlers for delegation settings — the web-mode mirror of the
//! Tauri commands in `commands::delegation`.
//!
//! Both endpoints share the same core helpers (`load_delegation_settings`,
//! `set_delegation_settings_core`) so the clamp + persist + broker
//! re-apply behavior stays identical across transports.

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::acp::delegation::types::DelegationProfileDocument;
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::delegation::{
    apply_profiles_to_broker, load_delegation_profiles, load_delegation_settings,
    set_delegation_bundle_core, set_delegation_profiles_core, set_delegation_settings_core,
    DelegationBundle, DelegationSettings,
};

pub async fn get_delegation_settings(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationSettings>, AppCommandError> {
    Ok(Json(load_delegation_settings(&state.db.conn).await))
}

#[derive(Deserialize)]
pub struct SetDelegationSettingsParams {
    pub settings: DelegationSettings,
}

pub async fn set_delegation_settings(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationSettingsParams>,
) -> Result<Json<DelegationSettings>, AppCommandError> {
    let saved =
        set_delegation_settings_core(&state.db.conn, &state.delegation_broker, params.settings)
            .await?;
    Ok(Json(saved))
}

pub async fn get_delegation_profiles(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationProfileDocument>, AppCommandError> {
    Ok(Json(load_delegation_profiles(&state.db.conn).await?))
}

#[derive(Deserialize)]
pub struct SetDelegationProfilesParams {
    pub document: DelegationProfileDocument,
}

pub async fn set_delegation_profiles(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationProfilesParams>,
) -> Result<Json<DelegationProfileDocument>, AppCommandError> {
    let saved = set_delegation_profiles_core(&state.db.conn, params.document).await?;
    apply_profiles_to_broker(&state.delegation_broker, &saved).await;
    Ok(Json(saved))
}

#[derive(Deserialize)]
pub struct SetDelegationBundleParams {
    pub bundle: DelegationBundle,
}

pub async fn set_delegation_bundle(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationBundleParams>,
) -> Result<Json<DelegationBundle>, AppCommandError> {
    let saved =
        set_delegation_bundle_core(&state.db.conn, &state.delegation_broker, params.bundle).await?;
    Ok(Json(saved))
}
