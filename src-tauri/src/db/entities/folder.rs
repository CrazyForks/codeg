use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "folder")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    #[sea_orm(unique)]
    pub path: String,
    pub git_branch: Option<String>,
    pub default_agent_type: Option<String>,
    pub last_opened_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
    pub is_open: bool,
    pub parent_branch: Option<String>,
    pub group_id: i32,
    pub sort_order_in_group: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::conversation::Entity")]
    Conversations,

    #[sea_orm(has_many = "super::folder_opened_conversation::Entity")]
    OpenedConversations,

    #[sea_orm(has_many = "super::folder_command::Entity")]
    FolderCommands,

    #[sea_orm(
        belongs_to = "super::folder_group::Entity",
        from = "Column::GroupId",
        to = "super::folder_group::Column::Id"
    )]
    Group,
}

impl Related<super::conversation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Conversations.def()
    }
}

impl Related<super::folder_opened_conversation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::OpenedConversations.def()
    }
}

impl Related<super::folder_command::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::FolderCommands.def()
    }
}

impl Related<super::folder_group::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
