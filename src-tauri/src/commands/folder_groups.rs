use sea_orm::DatabaseConnection;

use crate::app_error::AppCommandError;
use crate::db::service::folder_group_service;
#[cfg(feature = "tauri-runtime")]
use crate::db::AppDatabase;
use crate::models::{FolderGroup, FolderGroupDetail};
use crate::web::event_bridge::{emit_event, EventEmitter};

pub const FOLDER_GROUP_UPDATED_EVENT: &str = "folder-group-updated";

pub(crate) async fn list_folder_groups_core(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderGroupDetail>, AppCommandError> {
    folder_group_service::list_groups_with_folders(conn)
        .await
        .map_err(AppCommandError::from)
}

pub(crate) async fn create_folder_group_core(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    name: String,
) -> Result<FolderGroup, AppCommandError> {
    let group = folder_group_service::create_group(conn, name.trim())
        .await
        .map_err(AppCommandError::from)?;
    emit_event(emitter, FOLDER_GROUP_UPDATED_EVENT, &group);
    Ok(group)
}

pub(crate) async fn rename_folder_group_core(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    group_id: i32,
    name: String,
) -> Result<Option<FolderGroup>, AppCommandError> {
    let group = folder_group_service::rename_group(conn, group_id, name.trim())
        .await
        .map_err(AppCommandError::from)?;
    if let Some(ref g) = group {
        emit_event(emitter, FOLDER_GROUP_UPDATED_EVENT, g);
    }
    Ok(group)
}

pub(crate) async fn remove_folder_group_core(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    group_id: i32,
) -> Result<u64, AppCommandError> {
    let cascaded = folder_group_service::remove_group(conn, group_id)
        .await
        .map_err(AppCommandError::from)?;
    emit_event(
        emitter,
        FOLDER_GROUP_UPDATED_EVENT,
        &serde_json::json!({ "id": group_id, "removed": true, "cascaded_folders": cascaded }),
    );
    Ok(cascaded)
}

pub(crate) async fn reorder_folder_groups_core(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    ordered_ids: Vec<i32>,
) -> Result<(), AppCommandError> {
    folder_group_service::reorder_groups(conn, &ordered_ids)
        .await
        .map_err(AppCommandError::from)?;
    emit_event(
        emitter,
        FOLDER_GROUP_UPDATED_EVENT,
        &serde_json::json!({ "reordered": ordered_ids }),
    );
    Ok(())
}

pub(crate) async fn reorder_folders_in_group_core(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    group_id: i32,
    ordered_folder_ids: Vec<i32>,
) -> Result<(), AppCommandError> {
    folder_group_service::reorder_folders_in_group(conn, group_id, &ordered_folder_ids)
        .await
        .map_err(AppCommandError::from)?;
    emit_event(
        emitter,
        FOLDER_GROUP_UPDATED_EVENT,
        &serde_json::json!({ "group_id": group_id, "reordered_folders": ordered_folder_ids }),
    );
    Ok(())
}

// ── Tauri command wrappers ─────────────────────────────────────────────────

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_folder_groups(
    db: tauri::State<'_, AppDatabase>,
) -> Result<Vec<FolderGroupDetail>, AppCommandError> {
    list_folder_groups_core(&db.conn).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn create_folder_group(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    name: String,
) -> Result<FolderGroup, AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    create_folder_group_core(&db.conn, &emitter, name).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn rename_folder_group(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    group_id: i32,
    name: String,
) -> Result<Option<FolderGroup>, AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    rename_folder_group_core(&db.conn, &emitter, group_id, name).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn remove_folder_group(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    group_id: i32,
) -> Result<u64, AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    remove_folder_group_core(&db.conn, &emitter, group_id).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn reorder_folder_groups(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    ordered_ids: Vec<i32>,
) -> Result<(), AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    reorder_folder_groups_core(&db.conn, &emitter, ordered_ids).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn reorder_folders_in_group(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    group_id: i32,
    ordered_folder_ids: Vec<i32>,
) -> Result<(), AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    reorder_folders_in_group_core(&db.conn, &emitter, group_id, ordered_folder_ids).await
}
