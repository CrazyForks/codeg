use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::folder_groups as group_commands;
use crate::models::{FolderGroup, FolderGroupDetail};

pub async fn list_folder_groups(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<FolderGroupDetail>>, AppCommandError> {
    let result = group_commands::list_folder_groups_core(&state.db.conn).await?;
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct CreateFolderGroupParams {
    pub name: String,
}

pub async fn create_folder_group(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CreateFolderGroupParams>,
) -> Result<Json<FolderGroup>, AppCommandError> {
    let group =
        group_commands::create_folder_group_core(&state.db.conn, &state.emitter, params.name)
            .await?;
    Ok(Json(group))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFolderGroupParams {
    pub group_id: i32,
    pub name: String,
}

pub async fn rename_folder_group(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<RenameFolderGroupParams>,
) -> Result<Json<Option<FolderGroup>>, AppCommandError> {
    let group = group_commands::rename_folder_group_core(
        &state.db.conn,
        &state.emitter,
        params.group_id,
        params.name,
    )
    .await?;
    Ok(Json(group))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFolderGroupParams {
    pub group_id: i32,
}

pub async fn remove_folder_group(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<RemoveFolderGroupParams>,
) -> Result<Json<u64>, AppCommandError> {
    let cascaded =
        group_commands::remove_folder_group_core(&state.db.conn, &state.emitter, params.group_id)
            .await?;
    Ok(Json(cascaded))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderFolderGroupsParams {
    pub ordered_ids: Vec<i32>,
}

pub async fn reorder_folder_groups(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ReorderFolderGroupsParams>,
) -> Result<Json<()>, AppCommandError> {
    group_commands::reorder_folder_groups_core(
        &state.db.conn,
        &state.emitter,
        params.ordered_ids,
    )
    .await?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderFoldersInGroupParams {
    pub group_id: i32,
    pub ordered_folder_ids: Vec<i32>,
}

pub async fn reorder_folders_in_group(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ReorderFoldersInGroupParams>,
) -> Result<Json<()>, AppCommandError> {
    group_commands::reorder_folders_in_group_core(
        &state.db.conn,
        &state.emitter,
        params.group_id,
        params.ordered_folder_ids,
    )
    .await?;
    Ok(Json(()))
}
