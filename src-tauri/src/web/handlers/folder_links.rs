use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::folder_links as link_commands;
use crate::commands::folder_links::{FolderLinkDetail, FolderLinkPlan, FolderLinkRequest};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderIdParams {
    pub folder_id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewParams {
    pub folder_id: i32,
    pub paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateParams {
    pub folder_id: i32,
    pub items: Vec<FolderLinkRequest>,
    #[serde(default)]
    pub git_exclude: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameParams {
    pub link_id: i32,
    pub new_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkIdParams {
    pub link_id: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveParams {
    pub link_id: i32,
    #[serde(default)]
    pub delete_link: Option<bool>,
}

pub async fn list_folder_links(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<FolderIdParams>,
) -> Result<Json<Vec<FolderLinkDetail>>, AppCommandError> {
    Ok(Json(
        link_commands::list_folder_links_core(&state.db, params.folder_id).await?,
    ))
}

pub async fn preview_folder_links(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<PreviewParams>,
) -> Result<Json<Vec<FolderLinkPlan>>, AppCommandError> {
    Ok(Json(
        link_commands::preview_folder_links_core(&state.db, params.folder_id, params.paths).await?,
    ))
}

pub async fn create_folder_links(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CreateParams>,
) -> Result<Json<Vec<FolderLinkDetail>>, AppCommandError> {
    Ok(Json(
        link_commands::create_folder_links_core(
            &state.emitter,
            &state.db,
            params.folder_id,
            params.items,
            params.git_exclude.unwrap_or(true),
        )
        .await?,
    ))
}

pub async fn rename_folder_link(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<RenameParams>,
) -> Result<Json<FolderLinkDetail>, AppCommandError> {
    Ok(Json(
        link_commands::rename_folder_link_core(
            &state.emitter,
            &state.db,
            params.link_id,
            params.new_name,
        )
        .await?,
    ))
}

pub async fn repair_folder_link(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<LinkIdParams>,
) -> Result<Json<FolderLinkDetail>, AppCommandError> {
    Ok(Json(
        link_commands::repair_folder_link_core(&state.emitter, &state.db, params.link_id).await?,
    ))
}

pub async fn remove_folder_link(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<RemoveParams>,
) -> Result<Json<()>, AppCommandError> {
    link_commands::remove_folder_link_core(
        &state.emitter,
        &state.db,
        params.link_id,
        params.delete_link.unwrap_or(true),
    )
    .await?;
    Ok(Json(()))
}
