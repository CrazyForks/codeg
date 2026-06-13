use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::db::entities::loop_link::{self, LinkKind};
use crate::db::error::DbError;
use crate::models::loops::LoopLinkRow;

pub fn to_link_row(m: loop_link::Model) -> LoopLinkRow {
    LoopLinkRow {
        id: m.id,
        from_artifact_id: m.from_artifact_id,
        to_artifact_id: m.to_artifact_id,
        kind: m.kind,
    }
}

/// Idempotent: a repeated `(from, to, kind)` triple returns the existing edge
/// instead of inserting a duplicate (also guarded by `uniq_loop_link`).
pub async fn create_link(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    from_artifact_id: i32,
    to_artifact_id: i32,
    kind: LinkKind,
) -> Result<loop_link::Model, DbError> {
    if let Some(existing) = loop_link::Entity::find()
        .filter(loop_link::Column::FromArtifactId.eq(from_artifact_id))
        .filter(loop_link::Column::ToArtifactId.eq(to_artifact_id))
        .filter(loop_link::Column::Kind.eq(kind))
        .one(conn)
        .await?
    {
        return Ok(existing);
    }
    Ok(loop_link::ActiveModel {
        space_id: Set(space_id),
        from_artifact_id: Set(from_artifact_id),
        to_artifact_id: Set(to_artifact_id),
        kind: Set(kind),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}
