//! Human-driven engine actions (§4.6): trigger / pause / resume / cancel.
//!
//! These are the only points where a person steers a loop; everything else is
//! engine-autonomous. Each is a small, DB-authoritative state transition layered
//! on the driver registry:
//! - **trigger**: pending → running; create the issue worktree; start a driver.
//! - **pause**: running → paused(manual); stop the driver. In-flight agents are
//!   left alive — a pause halts *new* dispatch, it does not kill running work.
//! - **resume**: paused → running; start a fresh driver.
//! - **cancel**: → cancelled; stop the driver, invalidate every in-flight
//!   iteration's capability token (so the host rejects late submissions), and
//!   remove the worktree. (Killing the agent subprocess is M2.2; M2.1 closes the
//!   DB state and the worktree.)
//!
//! Every transition is guarded: a miss (the issue is not in the expected source
//! state) surfaces as [`LoopError::Conflict`], which the frontend retries.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveEnum, ColumnTrait, EntityTrait, QueryFilter};

use crate::db::entities::loop_issue::{self, IssueStatus, PauseReason};
use crate::db::entities::loop_iteration::{self, IterationStatus};
use crate::db::service::folder_service;
use crate::db::service::loop_service::{issue, space};

use crate::loop_engine::transitions::cas_issue_status;
use crate::loop_engine::{worktree, LoopEngine, LoopError};

impl LoopEngine {
    /// Trigger a pending issue: create its worktree, flip it running, and start
    /// the driver. The worktree is created *before* the status flip so a non-git
    /// repo (or any git failure) leaves the issue cleanly `pending`, retryable.
    pub async fn trigger_issue(self: &Arc<Self>, issue_id: i32) -> Result<(), LoopError> {
        let issue = issue::get_issue(&self.db.conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        if issue.status != IssueStatus::Pending {
            return Err(LoopError::Conflict);
        }
        // Validates the space repo is git, creates the worktree + branch, and
        // records the merge base on the issue (idempotent).
        worktree::ensure_worktree(&self.db.conn, &self.data_dir, issue_id).await?;
        if !cas_issue_status(
            &self.db.conn,
            issue_id,
            IssueStatus::Pending,
            IssueStatus::Running,
        )
        .await?
        {
            return Err(LoopError::Conflict);
        }
        self.start_issue(issue_id).await;
        Ok(())
    }

    /// Pause a running issue: halt new dispatch without killing in-flight agents.
    /// `stop_issue` removes the driver from the registry synchronously, so a
    /// follow-up resume always spawns a fresh driver (no handoff race).
    pub async fn pause_issue(&self, issue_id: i32) -> Result<(), LoopError> {
        let res = loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Paused.to_value()),
            )
            .col_expr(
                loop_issue::Column::PauseReason,
                Expr::value(PauseReason::Manual.to_value()),
            )
            .col_expr(loop_issue::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .filter(loop_issue::Column::Status.eq(IssueStatus::Running))
            .exec(&self.db.conn)
            .await?;
        if res.rows_affected != 1 {
            return Err(LoopError::Conflict);
        }
        self.stop_issue(issue_id).await;
        Ok(())
    }

    /// Resume a paused issue: clear the pause reason and start a fresh driver,
    /// which re-evaluates the frontier (picking up any progress made while the
    /// in-flight iteration finished during the pause).
    pub async fn resume_issue(self: &Arc<Self>, issue_id: i32) -> Result<(), LoopError> {
        let res = loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Running.to_value()),
            )
            .col_expr(
                loop_issue::Column::PauseReason,
                Expr::value(Option::<String>::None),
            )
            .col_expr(loop_issue::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .filter(loop_issue::Column::Status.eq(IssueStatus::Paused))
            .exec(&self.db.conn)
            .await?;
        if res.rows_affected != 1 {
            return Err(LoopError::Conflict);
        }
        self.start_issue(issue_id).await;
        Ok(())
    }

    /// Cancel an issue from any non-terminal state: close it, stop the driver,
    /// invalidate in-flight tokens, and remove the worktree.
    pub async fn cancel_issue(&self, issue_id: i32) -> Result<(), LoopError> {
        let now = Utc::now();
        let res = loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Cancelled.to_value()),
            )
            .col_expr(loop_issue::Column::EndedAt, Expr::value(now))
            .col_expr(loop_issue::Column::UpdatedAt, Expr::value(now))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .filter(loop_issue::Column::Status.is_in([
                IssueStatus::Pending,
                IssueStatus::Running,
                IssueStatus::Paused,
                IssueStatus::Blocked,
            ]))
            .exec(&self.db.conn)
            .await?;
        if res.rows_affected != 1 {
            return Err(LoopError::Conflict);
        }
        // Stop the driver, then invalidate every in-flight iteration: marking
        // them cancelled releases their leases AND makes the host reject any late
        // capability-token submission (ingest requires a `running` iteration).
        self.stop_issue(issue_id).await;
        loop_iteration::Entity::update_many()
            .col_expr(
                loop_iteration::Column::Status,
                Expr::value(IterationStatus::Cancelled.to_value()),
            )
            .col_expr(loop_iteration::Column::EndedAt, Expr::value(now))
            .filter(loop_iteration::Column::IssueId.eq(issue_id))
            .filter(
                loop_iteration::Column::Status
                    .is_in([IterationStatus::Queued, IterationStatus::Running]),
            )
            .exec(&self.db.conn)
            .await?;
        self.remove_issue_worktree(issue_id).await;
        Ok(())
    }

    /// Best-effort removal of an issue's git worktree (directory + admin entry).
    /// The hidden folder row and its iteration conversations are kept for audit;
    /// a cancelled issue's driver never restarts, so a stale `worktree_folder_id`
    /// is never read again. Any failure is logged, not fatal — the cancel's DB
    /// closure already succeeded.
    async fn remove_issue_worktree(&self, issue_id: i32) {
        let conn = &self.db.conn;
        let Ok(Some(issue)) = issue::get_issue(conn, issue_id).await else {
            return;
        };
        let Some(folder_id) = issue.worktree_folder_id else {
            return;
        };
        let Ok(Some(folder)) = folder_service::get_folder_by_id(conn, folder_id).await else {
            return;
        };
        if !Path::new(&folder.path).exists() {
            return;
        }
        let Ok(Some(space_row)) = space::get_space(conn, issue.space_id).await else {
            return;
        };
        let Ok(Some(repo)) = folder_service::get_folder_by_id(conn, space_row.folder_id).await
        else {
            return;
        };
        if let Err(e) =
            worktree::remove_worktree(Path::new(&repo.path), Path::new(&folder.path)).await
        {
            eprintln!("[loop] cancel: remove worktree {} failed: {e}", folder.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::Stage;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{cas_iteration_status, try_claim_iteration, IterationClaim};
    use crate::models::loops::IssueConfig;
    use crate::web::event_bridge::EventEmitter;

    /// Build an engine + a single issue already marked `running` (without going
    /// through trigger, so no worktree or driver is created — the pause/cancel
    /// paths under test never need one).
    async fn setup() -> (Arc<LoopEngine>, sea_orm::DatabaseConnection, i32, i32) {
        let db = fresh_in_memory_db().await;
        let conn = db.conn.clone();
        let folder_id = seed_folder(&db, "/tmp/loop-actions").await;
        let space = space::create_space(&conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            &IssueConfig::default(),
        )
        .await
        .unwrap();
        cas_issue_status(&conn, issue.row.id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            std::path::PathBuf::from("/tmp/loop-actions-data"),
            EventEmitter::Noop,
        );
        (engine, conn, space.id, issue.row.id)
    }

    #[tokio::test]
    async fn pause_sets_manual_reason_then_conflicts() {
        let (engine, conn, _space, issue_id) = setup().await;
        engine.pause_issue(issue_id).await.unwrap();

        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Paused);
        assert_eq!(issue.pause_reason, Some(PauseReason::Manual));

        // A second pause (no longer running) is a conflict, not a silent no-op.
        assert!(matches!(
            engine.pause_issue(issue_id).await,
            Err(LoopError::Conflict)
        ));
    }

    #[tokio::test]
    async fn cancel_closes_issue_and_invalidates_in_flight_token() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // An in-flight iteration holding a lease + a live capability token.
        let iter = try_claim_iteration(
            &conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Triage,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "live-token".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        cas_iteration_status(&conn, iter.id, IterationStatus::Queued, IterationStatus::Running)
            .await
            .unwrap();

        engine.cancel_issue(issue_id).await.unwrap();

        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Cancelled);
        assert!(issue.ended_at.is_some());
        let it = loop_iteration::Entity::find_by_id(iter.id)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            it.status,
            IterationStatus::Cancelled,
            "the in-flight token is invalidated so the host rejects late writes"
        );
    }

    #[tokio::test]
    async fn cancel_works_from_paused_then_conflicts_when_terminal() {
        let (engine, conn, _space, issue_id) = setup().await;
        engine.pause_issue(issue_id).await.unwrap();
        engine.cancel_issue(issue_id).await.unwrap();
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Cancelled
        );
        // Cancelling an already-terminal issue is a conflict.
        assert!(matches!(
            engine.cancel_issue(issue_id).await,
            Err(LoopError::Conflict)
        ));
    }
}
