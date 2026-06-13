use std::collections::HashMap;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::db::entities::{loop_artifact, loop_issue, loop_iteration};
use crate::db::error::DbError;
use crate::models::loops::LoopIterationRow;

fn to_iteration_row(
    m: &loop_iteration::Model,
    issue_seq: i32,
    target_title: Option<String>,
) -> LoopIterationRow {
    LoopIterationRow {
        id: m.id,
        issue_id: m.issue_id,
        issue_seq,
        stage: m.stage,
        target_artifact_id: m.target_artifact_id,
        target_title,
        conversation_id: m.conversation_id,
        status: m.status,
        launched_by: m.launched_by,
        attempt: m.attempt,
        tokens_used: m.tokens_used,
        created_at: m.created_at,
        started_at: m.started_at,
        ended_at: m.ended_at,
    }
}

pub async fn get_iteration(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
) -> Result<Option<loop_iteration::Model>, DbError> {
    Ok(loop_iteration::Entity::find_by_id(id).one(conn).await?)
}

async fn target_titles(
    conn: &sea_orm::DatabaseConnection,
    iterations: &[loop_iteration::Model],
) -> Result<HashMap<i32, String>, DbError> {
    let ids: Vec<i32> = iterations
        .iter()
        .filter_map(|i| i.target_artifact_id)
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(loop_artifact::Entity::find()
        .filter(loop_artifact::Column::Id.is_in(ids))
        .all(conn)
        .await?
        .into_iter()
        .map(|a| (a.id, a.title))
        .collect())
}

pub async fn list_iterations(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    let issue_seq = loop_issue::Entity::find_by_id(issue_id)
        .one(conn)
        .await?
        .map(|i| i.seq_no)
        .unwrap_or(0);
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .order_by_desc(loop_iteration::Column::Id)
        .all(conn)
        .await?;
    let titles = target_titles(conn, &rows).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let title = m
                .target_artifact_id
                .and_then(|tid| titles.get(&tid).cloned());
            to_iteration_row(m, issue_seq, title)
        })
        .collect())
}

pub async fn list_iterations_for_space(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    let seqs: HashMap<i32, i32> = loop_issue::Entity::find()
        .filter(loop_issue::Column::SpaceId.eq(space_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|i| (i.id, i.seq_no))
        .collect();
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::SpaceId.eq(space_id))
        .order_by_desc(loop_iteration::Column::Id)
        .all(conn)
        .await?;
    let titles = target_titles(conn, &rows).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let title = m
                .target_artifact_id
                .and_then(|tid| titles.get(&tid).cloned());
            to_iteration_row(m, *seqs.get(&m.issue_id).unwrap_or(&0), title)
        })
        .collect())
}
