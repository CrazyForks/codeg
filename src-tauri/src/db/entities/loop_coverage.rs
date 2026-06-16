use sea_orm::entity::prelude::*;

/// Criterion-level coverage: a task artifact claims it satisfies a given
/// (acceptance) criterion. The unit of traceability — the driver's bounded
/// replan loop-back fires whenever a requirement criterion has no covering task.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_coverage")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub space_id: i32,
    pub task_artifact_id: i32,
    pub criterion_id: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
