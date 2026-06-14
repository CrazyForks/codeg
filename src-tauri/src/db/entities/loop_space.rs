use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_space")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// Bound root folder (must be a git repo). Plain column — cross-subsystem
    /// reference to `folder.id`, no FK.
    pub folder_id: i32,
    /// Space default `IssueConfig` (JSON), `NOT NULL` — every space stores a
    /// concrete config (the engine default is written at creation). Issues whose
    /// own `config` is `NULL` resolve against this at read time.
    pub default_config: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
