use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set,
};

use crate::db::entities::{folder, folder_group};
use crate::db::error::DbError;
use crate::models::{FolderGroup, FolderGroupDetail, FolderHistoryEntry};

fn to_model(m: folder_group::Model) -> FolderGroup {
    FolderGroup {
        id: m.id,
        name: m.name,
        sort_order: m.sort_order,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

fn to_folder_entry(m: folder::Model) -> FolderHistoryEntry {
    FolderHistoryEntry {
        id: m.id,
        path: m.path,
        name: m.name,
        last_opened_at: m.last_opened_at,
        group_id: m.group_id,
        sort_order_in_group: m.sort_order_in_group,
        git_branch: m.git_branch,
        is_open: m.is_open,
    }
}

/// Inserts a new folder_group with the given name. `sort_order` is appended
/// to the end of the current list (max sort_order + 1).
pub async fn create_group(conn: &DatabaseConnection, name: &str) -> Result<FolderGroup, DbError> {
    let now = Utc::now();
    let next_sort = next_sort_order(conn).await?;

    let active = folder_group::ActiveModel {
        id: NotSet,
        name: Set(name.to_string()),
        sort_order: Set(next_sort),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let inserted = active.insert(conn).await?;
    Ok(to_model(inserted))
}

async fn next_sort_order(conn: &DatabaseConnection) -> Result<i32, DbError> {
    let last = folder_group::Entity::find()
        .filter(folder_group::Column::DeletedAt.is_null())
        .order_by_desc(folder_group::Column::SortOrder)
        .one(conn)
        .await?;
    Ok(last.map(|g| g.sort_order + 1).unwrap_or(0))
}

/// Soft-delete a folder_group if it has no remaining non-deleted folders.
pub async fn remove_group_if_empty(conn: &DatabaseConnection, group_id: i32) -> Result<(), DbError> {
    let remaining = folder::Entity::find()
        .filter(folder::Column::GroupId.eq(group_id))
        .filter(folder::Column::DeletedAt.is_null())
        .count(conn)
        .await?;

    if remaining == 0 {
        let row = folder_group::Entity::find_by_id(group_id)
            .filter(folder_group::Column::DeletedAt.is_null())
            .one(conn)
            .await?;
        if let Some(row) = row {
            let now = Utc::now();
            let mut active: folder_group::ActiveModel = row.into();
            active.deleted_at = Set(Some(now));
            active.updated_at = Set(now);
            active.update(conn).await?;
        }
    }
    Ok(())
}

/// Returns the next sort_order_in_group for a given group_id.
pub async fn next_folder_sort_order(
    conn: &DatabaseConnection,
    group_id: i32,
) -> Result<i32, DbError> {
    let last = folder::Entity::find()
        .filter(folder::Column::GroupId.eq(group_id))
        .filter(folder::Column::DeletedAt.is_null())
        .order_by_desc(folder::Column::SortOrderInGroup)
        .one(conn)
        .await?;
    Ok(last.map(|f| f.sort_order_in_group + 1).unwrap_or(0))
}

/// Rename a group by id. No-op if the group is missing or already soft-deleted.
pub async fn rename_group(
    conn: &DatabaseConnection,
    group_id: i32,
    name: &str,
) -> Result<Option<FolderGroup>, DbError> {
    let row = folder_group::Entity::find_by_id(group_id)
        .filter(folder_group::Column::DeletedAt.is_null())
        .one(conn)
        .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let now = Utc::now();
            let mut active: folder_group::ActiveModel = row.into();
            active.name = Set(name.to_string());
            active.updated_at = Set(now);
            let updated = active.update(conn).await?;
            Ok(Some(to_model(updated)))
        }
    }
}

/// Soft-delete a group and all its non-deleted folders. Returns the count of
/// folders that were cascade-deleted.
pub async fn remove_group(
    conn: &DatabaseConnection,
    group_id: i32,
) -> Result<u64, DbError> {
    let row = folder_group::Entity::find_by_id(group_id)
        .filter(folder_group::Column::DeletedAt.is_null())
        .one(conn)
        .await?;
    let Some(row) = row else {
        return Ok(0);
    };

    let now = Utc::now();
    let cascaded = folder::Entity::update_many()
        .filter(folder::Column::GroupId.eq(group_id))
        .filter(folder::Column::DeletedAt.is_null())
        .col_expr(folder::Column::DeletedAt, sea_orm::sea_query::Expr::value(now))
        .col_expr(folder::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
        .col_expr(folder::Column::IsOpen, sea_orm::sea_query::Expr::value(false))
        .exec(conn)
        .await?
        .rows_affected;

    let mut active: folder_group::ActiveModel = row.into();
    active.deleted_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(conn).await?;

    Ok(cascaded)
}

/// Apply a caller-supplied ordering to the active groups. Only ids present in
/// the input are updated; unknown ids are silently skipped.
pub async fn reorder_groups(
    conn: &DatabaseConnection,
    ordered_ids: &[i32],
) -> Result<(), DbError> {
    let now = Utc::now();
    for (index, id) in ordered_ids.iter().enumerate() {
        let row = folder_group::Entity::find_by_id(*id)
            .filter(folder_group::Column::DeletedAt.is_null())
            .one(conn)
            .await?;
        if let Some(row) = row {
            let mut active: folder_group::ActiveModel = row.into();
            active.sort_order = Set(index as i32);
            active.updated_at = Set(now);
            active.update(conn).await?;
        }
    }
    Ok(())
}

/// Apply a caller-supplied ordering to the non-deleted folders inside a group.
pub async fn reorder_folders_in_group(
    conn: &DatabaseConnection,
    group_id: i32,
    ordered_folder_ids: &[i32],
) -> Result<(), DbError> {
    let now = Utc::now();
    for (index, folder_id) in ordered_folder_ids.iter().enumerate() {
        let row = folder::Entity::find_by_id(*folder_id)
            .filter(folder::Column::GroupId.eq(group_id))
            .filter(folder::Column::DeletedAt.is_null())
            .one(conn)
            .await?;
        if let Some(row) = row {
            let mut active: folder::ActiveModel = row.into();
            active.sort_order_in_group = Set(index as i32);
            active.updated_at = Set(now);
            active.update(conn).await?;
        }
    }
    Ok(())
}

/// List all non-deleted groups with their non-deleted folders nested.
#[allow(dead_code)]
pub async fn list_groups_with_folders(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderGroupDetail>, DbError> {
    let groups = folder_group::Entity::find()
        .filter(folder_group::Column::DeletedAt.is_null())
        .order_by_asc(folder_group::Column::SortOrder)
        .order_by_asc(folder_group::Column::Id)
        .all(conn)
        .await?;

    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let folders = folder::Entity::find()
            .filter(folder::Column::GroupId.eq(g.id))
            .filter(folder::Column::DeletedAt.is_null())
            .order_by_asc(folder::Column::SortOrderInGroup)
            .order_by_asc(folder::Column::Id)
            .all(conn)
            .await?;

        out.push(FolderGroupDetail {
            id: g.id,
            name: g.name,
            sort_order: g.sort_order,
            created_at: g.created_at,
            updated_at: g.updated_at,
            folders: folders.into_iter().map(to_folder_entry).collect(),
        });
    }
    Ok(out)
}
