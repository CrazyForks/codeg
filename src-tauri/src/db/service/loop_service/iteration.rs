use std::collections::HashMap;

use sea_orm::sea_query::Expr;
use sea_orm::{ActiveEnum, ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::db::entities::loop_iteration::IterationOutcome;
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
        outcome: m.outcome,
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

/// D12: the reason from the most recent implement iteration of `task_id` that
/// declared the task already complete (via `loop_task_complete`) AND routed there
/// as a genuine no-op, if any. The review briefing surfaces it so the reviewer
/// verifies the acceptance criteria against the current worktree HEAD rather than
/// expecting a fresh checkpoint commit to inspect.
///
/// Gated on `outcome = declared_complete` (Codex r1): an agent that calls
/// `loop_task_complete` but ALSO makes a real diff settles with
/// `outcome = succeeded` (the non-empty checkpoint path), so its reason must NOT
/// surface the misleading "no checkpoint commit" note. Only the actual empty-diff
/// declared path records `declared_complete`.
pub async fn latest_declared_completion_reason(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
    task_id: i32,
) -> Result<Option<String>, DbError> {
    Ok(loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(loop_iteration::Column::TargetArtifactId.eq(task_id))
        .filter(loop_iteration::Column::Stage.eq(loop_iteration::Stage::Implement))
        .filter(loop_iteration::Column::Outcome.eq(IterationOutcome::DeclaredComplete))
        .filter(loop_iteration::Column::AgentCompletionReason.is_not_null())
        .order_by_desc(loop_iteration::Column::Id)
        .one(conn)
        .await?
        .and_then(|m| m.agent_completion_reason))
}

/// D12: clear the declared-completion reason on ALL of a task's implement
/// iterations. Called when review REJECTS a declared no-op, so a stale claim can
/// never route a future empty attempt straight to review again (the next empty
/// diff must be treated as genuine no-progress).
pub async fn clear_declared_completion(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
    task_id: i32,
) -> Result<(), DbError> {
    loop_iteration::Entity::update_many()
        .col_expr(
            loop_iteration::Column::AgentCompletionReason,
            Expr::value(Option::<String>::None),
        )
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(loop_iteration::Column::TargetArtifactId.eq(task_id))
        .filter(loop_iteration::Column::Stage.eq(loop_iteration::Stage::Implement))
        .exec(conn)
        .await?;
    Ok(())
}

/// Write-once outcome (D11): set `outcome` only while it is still NULL. Returns
/// `true` iff it wrote. Making the column immutable once set means a stale /
/// CAS-lost `abandoned` write can never clobber a real `succeeded` / `empty_diff`
/// / `validation_failed` (Codex r2 C2). The bulk abandon paths additionally filter
/// on the iteration's active status, so they only touch unsettled (NULL) rows.
pub async fn set_iteration_outcome(
    conn: &impl sea_orm::ConnectionTrait,
    id: i32,
    outcome: IterationOutcome,
) -> Result<bool, DbError> {
    let res = loop_iteration::Entity::update_many()
        .col_expr(loop_iteration::Column::Outcome, Expr::value(outcome.to_value()))
        .filter(loop_iteration::Column::Id.eq(id))
        .filter(loop_iteration::Column::Outcome.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

async fn target_titles(
    conn: &impl sea_orm::ConnectionTrait,
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

/// In-flight (`queued`|`running`) iterations for an issue, ascending by id.
/// Powers the real-time DAG/board ghost nodes + stage rail (spec D1); rides on
/// `LoopDagView.live_iterations` so the graph view is a single authoritative fetch.
pub async fn list_live_for_issue(
    conn: &impl sea_orm::ConnectionTrait,
    issue_id: i32,
) -> Result<Vec<LoopIterationRow>, DbError> {
    use crate::db::entities::loop_iteration::IterationStatus;
    let issue_seq = loop_issue::Entity::find_by_id(issue_id)
        .one(conn)
        .await?
        .map(|i| i.seq_no)
        .unwrap_or(0);
    let rows = loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(
            loop_iteration::Column::Status
                .is_in([IterationStatus::Queued, IterationStatus::Running]),
        )
        .order_by_asc(loop_iteration::Column::Id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::{IterationStatus, Stage};
    use crate::db::service::loop_service::{artifact, issue, space};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{
        cas_iteration_status, try_claim_iteration, IterationClaim,
    };
    use crate::models::loops::IssueConfig;

    /// `list_live_for_issue` returns only `queued`|`running` iterations, carrying
    /// stage/target/title — the contract `list_dag.live_iterations` relies on.
    #[tokio::test]
    async fn list_live_returns_only_queued_and_running() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo").await;
        let sp = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(
            &db.conn,
            sp.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            sp.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();

        // A running design iteration → live (carries its target title).
        let running = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Design,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "t1".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cas_iteration_status(
            &db.conn,
            running.id,
            IterationStatus::Queued,
            IterationStatus::Running,
        )
        .await
        .unwrap());

        // A succeeded refine iteration (different stage avoids the active-uniq
        // index) → NOT live.
        let done = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Refine,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "t2".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cas_iteration_status(
            &db.conn,
            done.id,
            IterationStatus::Queued,
            IterationStatus::Running,
        )
        .await
        .unwrap());
        assert!(cas_iteration_status(
            &db.conn,
            done.id,
            IterationStatus::Running,
            IterationStatus::Succeeded,
        )
        .await
        .unwrap());

        let live = list_live_for_issue(&db.conn, iss.row.id).await.unwrap();
        assert_eq!(live.len(), 1, "only queued|running iterations are live");
        assert_eq!(live[0].id, running.id);
        assert_eq!(live[0].stage, Stage::Design);
        assert_eq!(live[0].target_artifact_id, Some(task.id));
        assert_eq!(live[0].target_title.as_deref(), Some("T"));
        assert_eq!(live[0].status, IterationStatus::Running);
    }

    #[tokio::test]
    async fn declared_completion_reason_round_trip_and_clear() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo").await;
        let sp = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(
            &db.conn,
            sp.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            sp.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::InProgress,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();

        // No declaration yet.
        assert_eq!(
            latest_declared_completion_reason(&db.conn, iss.row.id, task.id)
                .await
                .unwrap(),
            None
        );

        // An implement iteration declares completion. The declared no-op
        // settlement path (gates::finish_implement) records BOTH the reason and
        // `outcome = declared_complete` — mirror that here so the surfacing query
        // (which gates on the outcome, Codex r1) matches production.
        let it = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Implement,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "tok".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        loop_iteration::Entity::update_many()
            .col_expr(
                loop_iteration::Column::AgentCompletionReason,
                Expr::value("already satisfied"),
            )
            .filter(loop_iteration::Column::Id.eq(it.id))
            .exec(&db.conn)
            .await
            .unwrap();
        assert!(set_iteration_outcome(&db.conn, it.id, IterationOutcome::DeclaredComplete)
            .await
            .unwrap());

        assert_eq!(
            latest_declared_completion_reason(&db.conn, iss.row.id, task.id)
                .await
                .unwrap()
                .as_deref(),
            Some("already satisfied")
        );

        // Review rejection clears it → a future empty attempt is genuine no-progress.
        clear_declared_completion(&db.conn, iss.row.id, task.id)
            .await
            .unwrap();
        assert_eq!(
            latest_declared_completion_reason(&db.conn, iss.row.id, task.id)
                .await
                .unwrap(),
            None
        );
    }

    /// Codex r1 regression: an agent that calls `loop_task_complete` but ALSO
    /// makes a real diff settles with `outcome = succeeded` (the non-empty
    /// checkpoint path), not `declared_complete`. Its stale reason must NOT be
    /// surfaced — otherwise the review briefing would wrongly tell the reviewer
    /// "no checkpoint commit to inspect" for an iteration that did produce one.
    #[tokio::test]
    async fn declared_reason_not_surfaced_after_real_diff() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/repo").await;
        let sp = space::create_space(&db.conn, "S", folder).await.unwrap();
        let iss = issue::create_issue(
            &db.conn,
            sp.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            sp.id,
            iss.row.id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::InProgress,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();

        let it = try_claim_iteration(
            &db.conn,
            IterationClaim {
                space_id: sp.id,
                issue_id: iss.row.id,
                stage: Stage::Implement,
                target_artifact_id: Some(task.id),
                slot_no: None,
                capability_token: "tok".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        // Reason recorded (the agent called loop_task_complete) ...
        loop_iteration::Entity::update_many()
            .col_expr(
                loop_iteration::Column::AgentCompletionReason,
                Expr::value("thought it was done"),
            )
            .filter(loop_iteration::Column::Id.eq(it.id))
            .exec(&db.conn)
            .await
            .unwrap();
        // ... but the checkpoint found a real diff, so it settled `succeeded`.
        assert!(set_iteration_outcome(&db.conn, it.id, IterationOutcome::Succeeded)
            .await
            .unwrap());

        assert_eq!(
            latest_declared_completion_reason(&db.conn, iss.row.id, task.id)
                .await
                .unwrap(),
            None,
            "a real-diff iteration's reason must not surface as a declared no-op"
        );
    }
}
