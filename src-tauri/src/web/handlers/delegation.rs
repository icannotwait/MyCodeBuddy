//! HTTP handlers for delegation settings — the web-mode mirror of the
//! Tauri commands in `commands::delegation`.
//!
//! Both endpoints share the same core helpers (`load_delegation_settings`,
//! `set_delegation_settings_core`) so the clamp + persist + broker
//! re-apply behavior stays identical across transports.

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::acp::delegation::types::{DelegationProfileCatalog, DelegationProfileDocument};
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::delegation::{
    load_delegation_profile_catalog, load_delegation_profiles, load_delegation_settings,
    set_delegation_bundle_core, set_delegation_profiles_core, set_delegation_settings_core,
    DelegationBundle, DelegationSettings, DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
};
use crate::web::event_bridge::emit_event;

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
    let mutation = set_delegation_settings_core(
        &state.db.conn,
        &state.delegation_broker,
        &state.delegation_runtime_settings,
        &state.connection_manager,
        params.settings,
    )
    .await?;
    emit_event(
        &state.emitter,
        DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
        mutation.catalog,
    );
    Ok(Json(mutation.value))
}

pub async fn get_delegation_profiles(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationProfileDocument>, AppCommandError> {
    Ok(Json(load_delegation_profiles(&state.db.conn).await?))
}

pub async fn get_delegation_profile_catalog(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<DelegationProfileCatalog>, AppCommandError> {
    Ok(Json(
        load_delegation_profile_catalog(&state.db.conn).await?,
    ))
}

#[derive(Deserialize)]
pub struct SetDelegationProfilesParams {
    pub document: DelegationProfileDocument,
}

pub async fn set_delegation_profiles(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationProfilesParams>,
) -> Result<Json<DelegationProfileDocument>, AppCommandError> {
    let mutation = set_delegation_profiles_core(
        &state.db.conn,
        &state.delegation_broker,
        params.document,
    )
    .await?;
    emit_event(
        &state.emitter,
        DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
        mutation.catalog,
    );
    Ok(Json(mutation.value))
}

#[derive(Deserialize)]
pub struct SetDelegationBundleParams {
    pub bundle: DelegationBundle,
}

pub async fn set_delegation_bundle(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<SetDelegationBundleParams>,
) -> Result<Json<DelegationBundle>, AppCommandError> {
    let mutation = set_delegation_bundle_core(
        &state.db.conn,
        &state.delegation_broker,
        &state.delegation_runtime_settings,
        &state.connection_manager,
        params.bundle,
    )
    .await?;
    emit_event(
        &state.emitter,
        DELEGATION_PROFILE_CATALOG_CHANGED_EVENT,
        mutation.catalog,
    );
    Ok(Json(mutation.value))
}
