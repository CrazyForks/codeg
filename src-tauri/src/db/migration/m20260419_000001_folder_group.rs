use chrono::Utc;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Create folder_group table
        manager
            .create_table(
                Table::create()
                    .table(FolderGroup::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FolderGroup::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FolderGroup::Name).string().not_null())
                    .col(
                        ColumnDef::new(FolderGroup::SortOrder)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(FolderGroup::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FolderGroup::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FolderGroup::DeletedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_folder_group_deleted_sort")
                    .table(FolderGroup::Table)
                    .col(FolderGroup::DeletedAt)
                    .col(FolderGroup::SortOrder)
                    .to_owned(),
            )
            .await?;

        // 2. Add folder.group_id (nullable at DB level; backfilled below) and
        //    folder.sort_order_in_group (NOT NULL default 0).
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(ColumnDef::new(Folder::GroupId).integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(
                        ColumnDef::new(Folder::SortOrderInGroup)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // 3. Backfill: walk every existing folder row and create a dedicated
        //    folder_group for it, preserving deleted_at so soft-deleted folders
        //    keep their group-coupled state.
        let conn = manager.get_connection();
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S %:z").to_string();

        let folder_rows = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id, name, last_opened_at, deleted_at FROM folder \
                 ORDER BY (deleted_at IS NOT NULL), last_opened_at DESC, id ASC"
                    .to_string(),
            ))
            .await?;

        for (index, row) in folder_rows.iter().enumerate() {
            let folder_id: i32 = row.try_get("", "id")?;
            let name: String = row.try_get("", "name")?;
            let deleted_at: Option<String> = row.try_get("", "deleted_at").ok();
            let sort_order = index as i32;

            let insert_result = conn
                .execute(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "INSERT INTO folder_group (name, sort_order, created_at, updated_at, deleted_at) \
                     VALUES (?, ?, ?, ?, ?)",
                    [
                        name.into(),
                        sort_order.into(),
                        now_str.clone().into(),
                        now_str.clone().into(),
                        deleted_at.into(),
                    ],
                ))
                .await?;
            let group_id = insert_result.last_insert_id() as i32;

            conn.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE folder SET group_id = ? WHERE id = ?",
                [group_id.into(), folder_id.into()],
            ))
            .await?;
        }

        // 4. Create index on folder(group_id, sort_order_in_group) for the
        //    navigation tree queries.
        manager
            .create_index(
                Index::create()
                    .name("idx_folder_group_sort")
                    .table(Folder::Table)
                    .col(Folder::GroupId)
                    .col(Folder::SortOrderInGroup)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_folder_group_sort")
                    .table(Folder::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::SortOrderInGroup)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_folder_group_deleted_sort")
                    .table(FolderGroup::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(FolderGroup::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FolderGroup {
    Table,
    Id,
    Name,
    SortOrder,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum Folder {
    Table,
    GroupId,
    SortOrderInGroup,
}
