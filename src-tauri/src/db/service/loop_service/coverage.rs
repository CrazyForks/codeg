use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::db::entities::{loop_artifact, loop_coverage};
use crate::db::error::DbError;
use crate::models::loops::LoopCoverageRow;

pub fn to_coverage_row(m: loop_coverage::Model) -> LoopCoverageRow {
    LoopCoverageRow {
        id: m.id,
        task_artifact_id: m.task_artifact_id,
        criterion_id: m.criterion_id,
    }
}

/// Idempotent: a repeated `(task, criterion)` pair returns the existing row
/// instead of inserting a duplicate (also guarded by `uniq_loop_coverage`).
pub async fn create_coverage(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    task_artifact_id: i32,
    criterion_id: i32,
) -> Result<loop_coverage::Model, DbError> {
    if let Some(existing) = loop_coverage::Entity::find()
        .filter(loop_coverage::Column::TaskArtifactId.eq(task_artifact_id))
        .filter(loop_coverage::Column::CriterionId.eq(criterion_id))
        .one(conn)
        .await?
    {
        return Ok(existing);
    }
    Ok(loop_coverage::ActiveModel {
        space_id: Set(space_id),
        task_artifact_id: Set(task_artifact_id),
        criterion_id: Set(criterion_id),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

/// All coverage edges whose task artifact belongs to `issue_id`. Joined through
/// the artifact's `issue_id` (coverage carries only `space_id`, not `issue_id`).
pub async fn list_for_issue(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<LoopCoverageRow>, DbError> {
    let task_ids: Vec<i32> = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|m| m.id)
        .collect();
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(loop_coverage::Entity::find()
        .filter(loop_coverage::Column::TaskArtifactId.is_in(task_ids))
        .all(conn)
        .await?
        .into_iter()
        .map(to_coverage_row)
        .collect())
}
