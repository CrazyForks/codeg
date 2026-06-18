//! Human-driven engine actions (§4.6): trigger / pause / resume / cancel.
//!
//! These are the only points where a person steers a loop; everything else is
//! engine-autonomous. Each is a small, DB-authoritative state transition layered
//! on the driver registry:
//! - **trigger**: pending → running; create the issue worktree; start a driver.
//! - **pause**: running → paused(manual); stop the driver. In-flight agents are
//!   left alive — a pause halts *new* dispatch, it does not kill running work.
//! - **resume**: paused → running; start a fresh driver.
//! - **cancel**: → cancelled; stop the driver, kill every in-flight iteration's
//!   agent subprocess, invalidate its capability token (so the host rejects late
//!   submissions), and remove the worktree.
//!
//! The **merge gate** (§4.10) also lives here: [`LoopEngine::merge_issue`] lands
//! a finalized issue's loop branch onto its base branch under a per-repo lock,
//! with a stale-base check; a clean landing closes the issue, any fault blocks it
//! with an inbox card.
//!
//! Every transition is guarded: a miss (the issue is not in the expected source
//! state) surfaces as [`LoopError::Conflict`], which the frontend retries. The
//! merge gate is the exception — it is idempotent (already-`done` → `Ok`) and
//! returns the non-retryable [`LoopError::NotMergeable`] for other non-mergeable
//! states; see [`LoopEngine::merge_issue`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveEnum, ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_artifact_revision::ActorKind;
use crate::db::entities::loop_inbox_item::{self, InboxKind, InboxStatus};
use crate::db::entities::loop_issue::{self, IssueStatus, PauseReason};
use crate::db::entities::loop_iteration::{self, IterationStatus, Stage};
use crate::db::service::folder_service;
use crate::db::service::loop_service::{artifact, inbox, issue, space};
use crate::models::loops::{LoopChanged, LOOP_CHANGED_EVENT};
use crate::web::event_bridge::emit_event;

use crate::loop_engine::config_resolver::effective_config;
use crate::loop_engine::transitions::{
    cas_artifact_status, cas_issue_status, cas_task_force_done_no_op, clear_oscillation,
};
use crate::loop_engine::worktree::{self, MergeOutcome};
use crate::loop_engine::{LoopEngine, LoopError};

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
        // Stop the driver, kill the agent processes, then invalidate every
        // in-flight iteration: marking them cancelled releases their leases AND
        // makes the host reject any late capability-token submission (ingest
        // requires a `running` iteration). Killing precedes the worktree removal
        // so no agent is still writing into the tree as it is torn down.
        self.stop_issue(issue_id).await;
        self.kill_in_flight_agents(issue_id).await;
        loop_iteration::Entity::update_many()
            .col_expr(
                loop_iteration::Column::Status,
                Expr::value(IterationStatus::Cancelled.to_value()),
            )
            .col_expr(loop_iteration::Column::EndedAt, Expr::value(now))
            // D11: cancelled-before-settling → `abandoned` — COALESCE keeps any
            // real outcome already recorded, so the write-once invariant holds at
            // the write itself, not merely via the active-status filter (Codex r1).
            .col_expr(
                loop_iteration::Column::Outcome,
                Expr::col(loop_iteration::Column::Outcome)
                    .if_null(loop_iteration::IterationOutcome::Abandoned.to_value()),
            )
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
            tracing::warn!(path = %folder.path, error = %e, "cancel: remove worktree failed");
        }
        // Also remove any per-task / integrate worktrees of a parallel issue. Keep
        // their branches for audit — cancel is not a permanent delete.
        let _ =
            worktree::remove_issue_subtree(Path::new(&repo.path), Path::new(&folder.path), false)
                .await;
    }

    /// Tear down the OS processes of an issue's in-flight iterations. Each live
    /// iteration's `conversation_id` resolves to its agent connection;
    /// `disconnect` sends the connection its shutdown command, reaping the child.
    /// Best-effort: a connection that already exited just isn't found. Reads the
    /// iteration rows directly (independent of the subsequent cancel CAS).
    async fn kill_in_flight_agents(&self, issue_id: i32) {
        let in_flight = match loop_iteration::Entity::find()
            .filter(loop_iteration::Column::IssueId.eq(issue_id))
            .filter(
                loop_iteration::Column::Status
                    .is_in([IterationStatus::Queued, IterationStatus::Running]),
            )
            .all(&self.db.conn)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "cancel: load in-flight iterations failed");
                return;
            }
        };
        for it in in_flight {
            let Some(cid) = it.conversation_id else {
                continue;
            };
            if let Some(conn_id) = self.manager.find_connection_by_conversation_id(cid).await {
                if let Err(e) = self.manager.disconnect(&conn_id).await {
                    tracing::warn!(conn_id = %conn_id, error = %e, "cancel: disconnect failed");
                }
            }
        }
    }

    /// Land a finalized issue's work onto its base branch — the merge gate
    /// (§4.10). Invoked by `approve_merge` (the human gate) or the driver
    /// (auto-merge); both take the same per-repo lock and run the same stale-base
    /// checks. A clean landing closes the issue (`done`) and removes its
    /// worktree; any fault (conflict / dirty base / failed re-validation / missing
    /// base) blocks the issue with an inbox card naming the cause AND returns a
    /// [`LoopError::MergeFailed`] carrying the reason — never a silent success that
    /// would leave the issue stuck "running" with no visible explanation.
    ///
    /// **Idempotent and race-free.** Preconditions are evaluated *under* the
    /// per-repo lock (not before it), so two actors — the human gate and the
    /// driver's auto-merge, or two clicks across surfaces — cannot both pass the
    /// gate and race the landing. A second call after the issue is already `done`
    /// (a concurrent actor merged it) returns `Ok(())` and re-emits `merged`,
    /// rather than the misleading `Conflict`/"retry". Any other non-`running` or
    /// no-`result` state returns the non-retryable [`LoopError::NotMergeable`] and
    /// emits a resync so a stale "running" view refetches the true status.
    pub async fn merge_issue(&self, issue_id: i32) -> Result<(), LoopError> {
        let conn = &self.db.conn;

        // Resolve the base repo path first — ONLY to choose which per-repo lock to
        // take. The authoritative precondition check happens after the lock (below),
        // so this pre-lock read cannot cause a TOCTOU. The repo path is immutable
        // for a space (folder paths have no mutation path; `space.folder_id` is
        // set once), so both actors derive the same lock key.
        let issue_probe = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        let space_row = space::get_space(conn, issue_probe.space_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("space {}", issue_probe.space_id)))?;
        let repo = folder_service::get_folder_by_id(conn, space_row.folder_id)
            .await?
            .ok_or(LoopError::Detached)?;
        let repo_path = PathBuf::from(&repo.path);

        // Serialize merges per base repo, THEN evaluate preconditions under the
        // lock: two issues sharing a repo must not race their --no-ff landings, and
        // two actors on the same issue must collapse to one effective merge.
        let lock = self.repo_merge_lock(&repo_path).await;
        let _guard = lock.lock().await;

        // Authoritative re-read under the lock.
        let issue = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;

        // Idempotent: a concurrent actor (driver auto-merge or another click)
        // already landed and closed this issue. `done` is written ONLY by the
        // landing below and is terminal, so it unambiguously means "merged".
        // Report success and re-emit so a stale view converges — never "retry".
        if issue.status == IssueStatus::Done {
            self.emit_changed(issue.space_id, issue_id, "merged");
            return Ok(());
        }
        // Not mergeable: any other non-running state (blocked / cancelled / paused
        // / pending), or the live result has not passed integration (D6) — finalize
        // produced no result, or its whole-issue closure isn't verified
        // (`gate_decision(result, finalize) == Pass`). Emit a resync FIRST so a view
        // still showing "running" refetches the true status (the original
        // transition's event may have been missed), then return the non-retryable
        // error.
        let dag = artifact::list_dag(conn, issue_id).await?;
        let integration_passed =
            crate::loop_engine::gates::integration_passed(conn, &dag).await?;
        if issue.status != IssueStatus::Running || !integration_passed {
            self.emit_changed(issue.space_id, issue_id, "merge_unavailable");
            return Err(LoopError::NotMergeable);
        }

        let folder_id = issue
            .worktree_folder_id
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id} worktree")))?;
        let folder = folder_service::get_folder_by_id(conn, folder_id)
            .await?
            .ok_or(LoopError::Detached)?;
        let worktree_path = PathBuf::from(&folder.path);
        let branch = format!("loop/{}/issue-{}", issue.space_id, issue.seq_no);
        let base_branch = issue
            .base_branch
            .clone()
            .ok_or_else(|| LoopError::Git("issue has no recorded base branch".into()))?;
        let base_commit = issue
            .base_commit
            .clone()
            .ok_or_else(|| LoopError::Git("issue has no recorded base commit".into()))?;
        let config =
            crate::loop_engine::config_resolver::effective_config(&self.db.conn, &issue).await?;

        let outcome = worktree::merge_issue(
            &repo_path,
            &worktree_path,
            &branch,
            &base_branch,
            &base_commit,
            &config.validation_commands,
            config.iteration_timeout_secs,
        )
        .await?;

        // A non-`Merged` outcome means the landing could not happen. Surface the
        // concrete reason as an error — NEVER a silent success that leaves the
        // issue stuck "running" with no visible cause. Block the issue + file a
        // durable card so the fault is visible to BOTH the human gate and the
        // driver's auto-merge (which only logs the error) — no silent stall on
        // "running". Supersede any pending merge-approval card so the blocked
        // issue shows only the retry path, not a now-dead "approve".
        if !matches!(outcome, MergeOutcome::Merged { .. }) {
            let (reason, message, detail) = merge_fault_report(&outcome);
            cas_issue_status(conn, issue_id, IssueStatus::Running, IssueStatus::Blocked).await?;
            inbox::upsert_inbox(
                conn,
                issue.space_id,
                issue_id,
                None,
                InboxKind::Blocked,
                &format!("merge_blocked:{issue_id}"),
                serde_json::json!({ "reason": reason, "detail": detail }),
            )
            .await?;
            resolve_approval_card(
                conn,
                issue_id,
                &format!("merge:{issue_id}"),
                serde_json::json!({ "action": "merge_failed", "reason": reason }),
            )
            .await?;
            self.emit_changed(issue.space_id, issue_id, "blocked");
            self.wake(issue_id).await;
            return Err(LoopError::MergeFailed(message));
        }

        // Merged. Best-effort teardown; the DB update below is the source of truth —
        // a merged issue never restarts, so a stale folder/worktree is inert.
        let _ = worktree::remove_worktree(&repo_path, &worktree_path).await;
        // Drop any per-task / integrate worktrees + their branches (a parallel
        // issue's task work is now in base via the fan-in, so they are merged).
        let _ = worktree::remove_issue_subtree(&repo_path, &worktree_path, true).await;
        // The loop branch is now in base behind the --no-ff merge commit, so drop
        // it. Safe `-d`: git refuses if it is somehow not merged, so this can never
        // discard unlanded work.
        let _ = worktree::delete_branch(&repo_path, &branch, false).await;
        let _ = folder_service::remove_folder(conn, &folder.path).await;
        resolve_approval_card(
            conn,
            issue_id,
            &format!("merge:{issue_id}"),
            serde_json::json!({ "action": "merged" }),
        )
        .await?;
        let now = Utc::now();
        let landed = loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Done.to_value()),
            )
            .col_expr(loop_issue::Column::EndedAt, Expr::value(now))
            .col_expr(loop_issue::Column::UpdatedAt, Expr::value(now))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .filter(loop_issue::Column::Status.eq(IssueStatus::Running))
            .exec(conn)
            .await?;
        if landed.rows_affected != 1 {
            // Unreachable under the lock (status was a freshly-confirmed `running`
            // re-read); the git work already landed, so warn rather than fail —
            // failing would falsely imply nothing merged.
            tracing::warn!(
                issue_id,
                rows = landed.rows_affected,
                "merge: status CAS to done affected an unexpected row count after landing"
            );
        }
        self.emit_changed(issue.space_id, issue_id, "merged");
        // Nudge the driver: it re-ticks, sees the terminal status, and stops.
        self.wake(issue_id).await;
        // Release the per-repo merge lock BEFORE the best-effort reflect dispatch so
        // spawning the reflect agent can never block another issue's merge on this
        // repo. Reflect is post-merge memory consolidation (§4.4) — it must never
        // affect the merge, so it runs only after `done` is durably committed.
        drop(_guard);
        if let Ok(Some(done)) = issue::get_issue(conn, issue_id).await {
            self.dispatch_reflect_best_effort(&done).await;
        }
        Ok(())
    }

    /// Best-effort reflect dispatch for a completed (`Done`) issue. NEVER returns
    /// an error and NEVER changes issue status — reflect must not touch the merge.
    /// The single guard is the durable anchor (D12): if a `reflection` artifact
    /// already exists for the issue, do nothing (covers crash-after-commit + a
    /// no-op success). Bounded by `max_attempts`; exhaustion files a low-priority
    /// inbox card (D11). Runs in the base repo folder with a read-only briefing (D1).
    /// Called at the merge hook, on every reflect settle (uptime self-retry), and
    /// on boot recovery.
    pub(crate) async fn dispatch_reflect_best_effort(&self, issue: &loop_issue::Model) {
        use sea_orm::PaginatorTrait;
        let conn = &self.db.conn;
        // Anchor: already consolidated?
        match loop_artifact::Entity::find()
            .filter(loop_artifact::Column::IssueId.eq(issue.id))
            .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Reflection))
            .count(conn)
            .await
        {
            Ok(0) => {}
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(issue_id = issue.id, error = %e, "reflect: anchor check failed");
                return;
            }
        }
        let config = match crate::loop_engine::config_resolver::effective_config(conn, issue).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(issue_id = issue.id, error = %e, "reflect: config resolve failed");
                return;
            }
        };
        // Count only TERMINAL reflect attempts (exclude queued/running): an in-flight
        // reflect is not yet a spent attempt, so it can never trigger a premature
        // exhaustion card while it might still produce the artifact. A finished
        // reflect counts whether it Succeeded-without-artifact, Failed, Interrupted,
        // or Cancelled — the artifact (not the iteration status) is the real success
        // signal.
        let attempts = match loop_iteration::Entity::find()
            .filter(loop_iteration::Column::IssueId.eq(issue.id))
            .filter(loop_iteration::Column::Stage.eq(Stage::Reflect))
            .filter(loop_iteration::Column::Status.is_in([
                IterationStatus::Succeeded,
                IterationStatus::Failed,
                IterationStatus::Interrupted,
                IterationStatus::Cancelled,
            ]))
            .count(conn)
            .await
        {
            Ok(n) => n as u32,
            Err(e) => {
                tracing::warn!(issue_id = issue.id, error = %e, "reflect: attempt count failed");
                return;
            }
        };
        if config.max_attempts > 0 && attempts >= config.max_attempts {
            // Terminal: a dismissible, informational card (idempotent upsert). NOT
            // "blocked" — a `Done` issue is never mislabeled.
            let _ = inbox::upsert_inbox(
                conn,
                issue.space_id,
                issue.id,
                None,
                InboxKind::ReflectionFailed,
                &format!("reflect_failed:{}", issue.id),
                serde_json::json!({ "reason": "reflect_exhausted", "attempts": attempts }),
            )
            .await;
            self.emit_changed(issue.space_id, issue.id, "reflect_exhausted");
            return;
        }
        let folder_id = match space::get_space(conn, issue.space_id).await {
            Ok(Some(s)) => s.folder_id,
            _ => {
                tracing::warn!(issue_id = issue.id, "reflect: space/folder lookup failed");
                return;
            }
        };
        let spec = crate::loop_engine::driver::resolve_agent_spec(&config, Stage::Reflect);
        match self
            .dispatch_iteration(crate::loop_engine::dispatch::DispatchInput {
                space_id: issue.space_id,
                issue_id: issue.id,
                stage: Stage::Reflect,
                target_artifact_id: None,
                slot_no: None,
                attempt: attempts as i32,
                agent_type: spec.agent,
                mode_id: spec.mode_id,
                config_values: spec.config_values,
                worktree_folder_id: folder_id,
            })
            .await
        {
            Ok(Some(_)) => tracing::debug!(issue_id = issue.id, "reflect: dispatched"),
            Ok(None) => tracing::debug!(issue_id = issue.id, "reflect: lease already held"),
            Err(e) => {
                tracing::warn!(issue_id = issue.id, error = %e, "reflect: dispatch failed (best-effort)")
            }
        }
    }

    /// Approve the design gate (route=full): mark every design that is awaiting
    /// approval `done` and wake the driver, which then advances to planning.
    /// [`LoopError::Conflict`] when no design is awaiting (already approved /
    /// rejected, or none produced).
    pub async fn approve_design(&self, issue_id: i32) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let issue = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        let awaiting = awaiting_design_ids(conn, issue_id).await?;
        if awaiting.is_empty() {
            return Err(LoopError::Conflict);
        }
        for id in &awaiting {
            cas_artifact_status(conn, *id, ArtifactStatus::AwaitingApproval, ArtifactStatus::Done)
                .await?;
        }
        resolve_approval_card(
            conn,
            issue_id,
            &format!("design:{issue_id}"),
            serde_json::json!({ "action": "approve" }),
        )
        .await?;
        self.emit_changed(issue.space_id, issue_id, "design_approved");
        self.wake(issue_id).await;
        Ok(())
    }

    /// Reject the design gate: supersede every awaiting design (recording the
    /// reviewer's comment as a human revision so the re-dispatched design isn't
    /// blind) and wake the driver, which produces a fresh design. Conflict when
    /// no design is awaiting.
    pub async fn reject_design(
        &self,
        issue_id: i32,
        comment: Option<String>,
    ) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let issue = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        let awaiting = awaiting_design_ids(conn, issue_id).await?;
        if awaiting.is_empty() {
            return Err(LoopError::Conflict);
        }
        let note = comment.unwrap_or_default();
        for id in &awaiting {
            cas_artifact_status(
                conn,
                *id,
                ArtifactStatus::AwaitingApproval,
                ArtifactStatus::Superseded,
            )
            .await?;
            if !note.trim().is_empty() {
                artifact::add_revision(
                    conn,
                    *id,
                    &format!("[design rejected] {}", note.trim()),
                    ActorKind::Human,
                    None,
                )
                .await?;
            }
        }
        resolve_approval_card(
            conn,
            issue_id,
            &format!("design:{issue_id}"),
            serde_json::json!({ "action": "reject", "comment": note }),
        )
        .await?;
        self.emit_changed(issue.space_id, issue_id, "design_rejected");
        self.wake(issue_id).await;
        Ok(())
    }

    /// Retry a blocked issue — the inbox "retry" escape hatch. Re-arms every
    /// blocked task for a fresh implement run, marks the blocking cards handled,
    /// and puts the issue back to `running` under a fresh driver. Conflict when
    /// the issue is not `blocked`.
    ///
    /// Each non-oscillating blocked task is reset `blocked → pending` with its
    /// failure signature cleared AND its `attempt` reset to 0 (D13) — a deliberate
    /// fresh budget against `max_attempts` per retry. Oscillating tasks (a
    /// deterministic repeat) are EXCLUDED — they need an explicit override/force
    /// exit, not a plain retry — and their `oscillation_*` columns are preserved as
    /// a cross-retry probe. Issue-level blocks (dirty finalize, merge fault/reject)
    /// have no blocked task; retry simply re-drives so the engine re-evaluates.
    pub async fn retry_issue(self: &Arc<Self>, issue_id: i32) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let issue = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        if issue.status != IssueStatus::Blocked {
            // Stale action: the issue is no longer blocked (e.g. already terminal
            // while a view still shows "running"). Nudge subscribers to refetch the
            // true status, then report the conflict.
            self.emit_changed(issue.space_id, issue_id, "retry_unavailable");
            return Err(LoopError::Conflict);
        }
        // D13: the re-arm set excludes OSCILLATING tasks (a deterministic failure
        // that plain retry can't fix — those need an explicit override/force exit).
        let config = effective_config(conn, &issue).await?;
        let limit = config.oscillation_limit as i32;
        let blocked: Vec<loop_artifact::Model> = loop_artifact::Entity::find()
            .filter(loop_artifact::Column::IssueId.eq(issue_id))
            .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Task))
            .filter(loop_artifact::Column::Status.eq(ArtifactStatus::Blocked))
            .all(conn)
            .await?;
        let is_oscillating = |t: &loop_artifact::Model| {
            limit > 0 && t.oscillation_count >= limit && t.recent_failure_sig.is_some()
        };
        let rearm: Vec<i32> = blocked
            .iter()
            .filter(|t| !is_oscillating(t))
            .map(|t| t.id)
            .collect();
        // An issue-level block (triage_no_route / dependency / finalize_dirty /
        // merge_*) has NO blocked task — a plain retry must still re-drive it. Reject
        // ONLY when blocked tasks exist AND every one is oscillating (use override).
        if !blocked.is_empty() && rearm.is_empty() {
            self.emit_changed(issue.space_id, issue_id, "retry_unavailable");
            return Err(LoopError::Conflict);
        }
        // ── ONE transaction: issue anchor + task re-arm + card resolve commit
        // together, so the driver's atomic re-park (C9) can never observe a
        // half-applied "issue=running + task still blocked" state. SQLite serializes
        // this txn against C9's UPDATE, which then sees only pre- or post-mutation
        // state.
        let txn = conn.begin().await?;
        // Serialization gate: CAS issue blocked→running. A concurrent retry's loser
        // sees 0 rows → rollback + Conflict, having mutated nothing.
        if !cas_issue_status(&txn, issue_id, IssueStatus::Blocked, IssueStatus::Running).await? {
            txn.rollback().await?;
            return Err(LoopError::Conflict);
        }
        // Re-arm with a `status = Blocked` CAS filter so a task a concurrent
        // force-complete moved Blocked→Done is NOT clobbered back to pending; reset
        // the attempt budget (D13) but KEEP the oscillation columns (cross-retry
        // probe — only override/force/real-progress clear them).
        if !rearm.is_empty() {
            loop_artifact::Entity::update_many()
                .col_expr(
                    loop_artifact::Column::Status,
                    Expr::value(ArtifactStatus::Pending.to_value()),
                )
                .col_expr(loop_artifact::Column::Attempt, Expr::value(0))
                .col_expr(
                    loop_artifact::Column::LastFailureSig,
                    Expr::value(Option::<String>::None),
                )
                .col_expr(loop_artifact::Column::UpdatedAt, Expr::value(Utc::now()))
                .filter(loop_artifact::Column::Id.is_in(rearm.clone()))
                .filter(loop_artifact::Column::Status.eq(ArtifactStatus::Blocked))
                .exec(&txn)
                .await?;
        }
        // Resolve every pending Blocked card EXCEPT task-level `oscillation:` (those
        // clear only via override/force) — issue-level blocks + the re-armed tasks'
        // ordinary blockers, in one broad sweep.
        inbox::resolve_blocked_cards_except_oscillation(
            &txn,
            issue_id,
            serde_json::json!({ "action": "retry" }),
        )
        .await?;
        txn.commit().await?;
        self.emit_changed(issue.space_id, issue_id, "retried");
        self.start_issue(issue_id).await;
        Ok(())
    }

    /// D15: human force-complete of a blocked, empty-diff task — accept it as a
    /// no-op so a wedged issue can finish. Cause-guarded to the `empty_diff:implement`
    /// family ONLY (a validation/infra/abandoned cause is rejected: its reset tree
    /// also looks clean, so accepting it would pass off unimplemented work as done).
    /// A parallel task whose branch carries a committed delta is refused (the no-op
    /// would discard it). Anchors the issue `running`, marks the task Done(no_op),
    /// clears its oscillation epoch, resolves its blocker cards — all in one
    /// transaction — then lets the driver re-evaluate (finalize / re-park).
    pub async fn force_complete_task(self: &Arc<Self>, task_id: i32) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let task = loop_artifact::Entity::find_by_id(task_id)
            .one(conn)
            .await?
            .filter(|a| a.kind == ArtifactKind::Task)
            .ok_or_else(|| LoopError::NotFound(format!("task {task_id}")))?;
        if task.status != ArtifactStatus::Blocked {
            self.emit_changed(task.space_id, task.issue_id, "force_complete_unavailable");
            return Err(LoopError::Conflict);
        }
        // Cause guard (D15): gate on the CURRENT pending blocker card's failure_sig,
        // not the artifact's `recent/last_failure_sig` columns (which validation /
        // infra block paths leave stale). Only the `empty_diff:implement` family is
        // no-op-compatible; a task re-blocked for a real validation/infra failure is
        // rejected — and this holds in serial mode too, where the parallel
        // branch-at-base defence below does not run.
        let blocker_sig = inbox::task_blocker_failure_sig(conn, task.issue_id, task_id).await?;
        if !matches!(blocker_sig.as_deref(), Some(s) if s.starts_with("empty_diff:implement")) {
            self.emit_changed(task.space_id, task.issue_id, "force_complete_unavailable");
            return Err(LoopError::Conflict);
        }
        let issue = issue::get_issue(conn, task.issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {}", task.issue_id)))?;
        // Defence-in-depth (reads only, BEFORE the txn): never no-op away a real
        // committed delta. If the parallel task branch advanced past its base, refuse.
        if issue.execution_mode.as_deref() == Some("parallel") {
            if let Some(false) = worktree::task_branch_at_base(conn, &issue, task_id).await? {
                return Err(LoopError::Conflict);
            }
        }
        // ── ONE transaction: anchor + done CAS + clear + resolve, so C9's re-park
        // can never observe "issue=running + task still blocked" mid-action.
        let txn = conn.begin().await?;
        if let Err(e) = ensure_running_for_exit(&txn, &issue).await {
            txn.rollback().await?;
            return Err(e);
        }
        // Re-validate the cause INSIDE the serialized window (Codex r2). The pre-txn
        // guard above is only a fast-fail: a concurrent retry could have re-armed and
        // re-blocked this task for a DIFFERENT cause (validation/infra), or with a real
        // branch delta, between that read and the CAS — which only checks `status =
        // blocked`. Re-read the pending blocker card's failure_sig now that the write
        // lock is held; only the empty_diff family stays no-op-eligible. A retry that
        // re-armed the task resolves its old card, so a vanished card → None → reject.
        let live_sig = inbox::task_blocker_failure_sig(&txn, task.issue_id, task_id).await?;
        if !matches!(live_sig.as_deref(), Some(s) if s.starts_with("empty_diff:implement")) {
            txn.rollback().await?;
            self.emit_changed(task.space_id, task.issue_id, "force_complete_unavailable");
            return Err(LoopError::Conflict);
        }
        if !cas_task_force_done_no_op(&txn, task_id).await? {
            txn.rollback().await?;
            return Err(LoopError::Conflict);
        }
        clear_oscillation(&txn, task_id).await?;
        inbox::resolve_task_blocker_cards(
            &txn,
            issue.id,
            task_id,
            &["no_progress", "validation_blocked", "infra_failure", "oscillation"],
            serde_json::json!({ "action": "force_complete", "actor": "human" }),
        )
        .await?;
        txn.commit().await?;
        self.emit_changed(issue.space_id, issue.id, "force_completed");
        // Issue is running → its driver re-evaluates (finalize / re-park). Never
        // self-finalize here — the driver owns frontier / fan-in / re-park.
        self.start_issue(issue.id).await;
        Ok(())
    }

    /// D17: human override of an oscillation breaker — clear the epoch and re-arm
    /// the task for a fresh attempt budget (distinct from a plain retry, which
    /// deliberately EXCLUDES oscillating tasks). Anchors the issue `running`,
    /// re-arms the task (pending, attempt 0, oscillation cleared), resolves ALL its
    /// blocker cards (including `oscillation:`) — one transaction — then re-drives.
    pub async fn override_oscillation(self: &Arc<Self>, task_id: i32) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let task = loop_artifact::Entity::find_by_id(task_id)
            .one(conn)
            .await?
            .filter(|a| a.kind == ArtifactKind::Task)
            .ok_or_else(|| LoopError::NotFound(format!("task {task_id}")))?;
        if task.status != ArtifactStatus::Blocked {
            self.emit_changed(task.space_id, task.issue_id, "override_unavailable");
            return Err(LoopError::Conflict);
        }
        // D17 precondition (Codex r2): override is for breaker-promoted tasks ONLY —
        // require a pending `oscillation:` card, so this endpoint can't be used as a
        // generic blocked-task reset (which is exactly what `retry` deliberately
        // EXCLUDES). The UI only offers it on oscillation cards; enforce it server-side.
        if !inbox::has_pending_oscillation_card(conn, task.issue_id, task_id).await? {
            self.emit_changed(task.space_id, task.issue_id, "override_unavailable");
            return Err(LoopError::Conflict);
        }
        let issue = issue::get_issue(conn, task.issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {}", task.issue_id)))?;
        let txn = conn.begin().await?;
        if let Err(e) = ensure_running_for_exit(&txn, &issue).await {
            txn.rollback().await?;
            return Err(e);
        }
        // Gate = re-arm CAS filtered on status='blocked' (double-click's 2nd call
        // affects 0 rows → rollback + Conflict). Reset epoch + attempt + sigs.
        let res = loop_artifact::Entity::update_many()
            .col_expr(
                loop_artifact::Column::Status,
                Expr::value(ArtifactStatus::Pending.to_value()),
            )
            .col_expr(loop_artifact::Column::Attempt, Expr::value(0))
            .col_expr(
                loop_artifact::Column::LastFailureSig,
                Expr::value(Option::<String>::None),
            )
            .col_expr(loop_artifact::Column::OscillationCount, Expr::value(0))
            .col_expr(
                loop_artifact::Column::RecentFailureSig,
                Expr::value(Option::<String>::None),
            )
            .col_expr(loop_artifact::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(loop_artifact::Column::Id.eq(task_id))
            .filter(loop_artifact::Column::Status.eq(ArtifactStatus::Blocked))
            .exec(&txn)
            .await?;
        if res.rows_affected != 1 {
            txn.rollback().await?;
            return Err(LoopError::Conflict);
        }
        inbox::resolve_task_blocker_cards(
            &txn,
            issue.id,
            task_id,
            &["no_progress", "validation_blocked", "infra_failure", "oscillation"],
            serde_json::json!({ "action": "override_oscillation", "actor": "human" }),
        )
        .await?;
        txn.commit().await?;
        self.emit_changed(issue.space_id, issue.id, "oscillation_override");
        self.start_issue(issue.id).await;
        Ok(())
    }

    /// Top up a budget-paused issue's token budget and resume it — the inbox
    /// "add budget" escape hatch. `additional` (clamped to ≥ 0) is added to the
    /// current `token_budget`; the budget card is marked handled and the issue
    /// resumes under a fresh driver. Conflict when the issue is not `paused`.
    pub async fn add_budget(self: &Arc<Self>, issue_id: i32, additional: i64) -> Result<(), LoopError> {
        let conn = &self.db.conn;
        let issue = issue::get_issue(conn, issue_id)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("issue {issue_id}")))?;
        if issue.status != IssueStatus::Paused {
            return Err(LoopError::Conflict);
        }
        let new_budget = issue.token_budget.unwrap_or(0).saturating_add(additional.max(0));
        loop_issue::Entity::update_many()
            .col_expr(loop_issue::Column::TokenBudget, Expr::value(new_budget))
            .col_expr(loop_issue::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(conn)
            .await?;
        resolve_cards_of_kind(
            conn,
            issue_id,
            InboxKind::BudgetExhausted,
            serde_json::json!({ "action": "add_budget", "additional": additional }),
        )
        .await?;
        self.emit_changed(issue.space_id, issue_id, "budget_added");
        // Flip paused → running, clear the pause reason, and start a fresh driver.
        self.resume_issue(issue_id).await
    }

    /// Emit the coarse `loop://changed` refetch signal for an issue.
    pub(crate) fn emit_changed(&self, space_id: i32, issue_id: i32, kind: &str) {
        emit_event(
            &self.emitter,
            LOOP_CHANGED_EVENT,
            LoopChanged {
                v: 1,
                space_id,
                issue_id: Some(issue_id),
                subject_kind: "issue".to_string(),
                subject_id: issue_id,
                kind: kind.to_string(),
            },
        );
    }
}

/// The issue's design artifacts currently `awaiting_approval`.
async fn awaiting_design_ids(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
) -> Result<Vec<i32>, LoopError> {
    let dag = artifact::list_dag(conn, issue_id).await?;
    Ok(dag
        .artifacts
        .iter()
        .filter(|a| {
            a.kind == ArtifactKind::Design && a.status == ArtifactStatus::AwaitingApproval
        })
        .map(|a| a.id)
        .collect())
}

/// Mark the pending approval inbox card (`kind=approval`, `subject_key=subject`)
/// for an issue handled. No-op when none exists — auto paths and direct calls
/// run fine without a card.
async fn resolve_approval_card(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
    subject: &str,
    resolution: serde_json::Value,
) -> Result<(), LoopError> {
    if let Some(card) = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::IssueId.eq(issue_id))
        .filter(loop_inbox_item::Column::Kind.eq(InboxKind::Approval))
        .filter(loop_inbox_item::Column::SubjectKey.eq(subject.to_string()))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .one(conn)
        .await?
    {
        inbox::handle_inbox(conn, card.id, resolution).await?;
    }
    Ok(())
}

/// Mark every pending inbox card of `kind` for an issue handled. A blocked issue
/// may carry more than one card (e.g. several `no_progress:{task}` keys), so the
/// retry / add-budget escape hatches clear them all in one resolution.
/// Anchor an issue `running` before a human exit action mutates its tasks (D15/D17),
/// so a post-commit crash is boot-restartable and the driver re-evaluates. Takes
/// `&impl ConnectionTrait` to run inside the exit-action transaction.
///
/// Authoritative re-anchor (Codex r3): it does NOT trust the caller's (possibly
/// stale) `issue.status`. One guarded write sets `running` for any issue currently
/// `running` OR `blocked`. This both (a) re-anchors an issue a concurrent driver
/// re-parked `running → blocked` after the caller read it, and (b) acquires the
/// issue-row write lock here — BEFORE the live blocker-card read and task CAS that
/// follow in the same transaction — so a re-park can no longer flip the issue
/// between those reads and the commit (which would otherwise strand an all-done
/// issue `blocked` with no actionable card). `updated_at` always changes, so a
/// still-`running` issue still counts as one affected row; a terminal / paused issue
/// matches zero rows → `Conflict`.
async fn ensure_running_for_exit(
    conn: &impl sea_orm::ConnectionTrait,
    issue: &loop_issue::Model,
) -> Result<(), LoopError> {
    use IssueStatus::*;
    let res = loop_issue::Entity::update_many()
        .col_expr(loop_issue::Column::Status, Expr::value(Running.to_value()))
        .col_expr(loop_issue::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(loop_issue::Column::Id.eq(issue.id))
        .filter(loop_issue::Column::Status.is_in([Running, Blocked]))
        .exec(conn)
        .await?;
    if res.rows_affected == 1 {
        Ok(())
    } else {
        Err(LoopError::Conflict)
    }
}

async fn resolve_cards_of_kind(
    conn: &sea_orm::DatabaseConnection,
    issue_id: i32,
    kind: InboxKind,
    resolution: serde_json::Value,
) -> Result<(), LoopError> {
    let cards = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::IssueId.eq(issue_id))
        .filter(loop_inbox_item::Column::Kind.eq(kind))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .all(conn)
        .await?;
    for card in cards {
        inbox::handle_inbox(conn, card.id, resolution.clone()).await?;
    }
    Ok(())
}

/// Map a non-`Merged` outcome to `(inbox reason code, user-facing message,
/// diagnostic detail)`. The reason code keys the inbox card; the message is the
/// error toast the user sees; the detail carries the git/validation output.
fn merge_fault_report(outcome: &MergeOutcome) -> (&'static str, String, String) {
    match outcome {
        MergeOutcome::BaseGone => (
            "base_gone",
            "The base branch no longer exists.".to_string(),
            "base branch no longer exists".to_string(),
        ),
        MergeOutcome::BaseDirty => (
            "base_dirty",
            "The base repository has uncommitted changes to tracked files. Commit or stash them, then merge again."
                .to_string(),
            "base repo working tree has uncommitted tracked changes".to_string(),
        ),
        MergeOutcome::Conflict { stage, detail } => {
            let (reason, message) = if *stage == "integrate" {
                (
                    "merge_conflict_integrate",
                    "Merge conflict while integrating the latest base into the issue branch.",
                )
            } else {
                (
                    "merge_conflict",
                    "Merge conflict while landing the issue branch onto the base branch.",
                )
            };
            (reason, message.to_string(), detail.clone())
        }
        MergeOutcome::RevalidationFailed { output } => (
            "revalidation_failed",
            "Re-validation failed on the merged result.".to_string(),
            output.clone(),
        ),
        // Not reached: the success arm is handled before this is called.
        MergeOutcome::Merged { .. } => ("merged", "Merge failed.".to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::db::entities::loop_artifact::{self, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_issue::IssuePriority;
    use crate::db::entities::loop_iteration::Stage;
    use crate::db::test_helpers::{fresh_disk_db, fresh_in_memory_db, seed_folder};
    use crate::loop_engine::transitions::{cas_iteration_status, try_claim_iteration, IterationClaim};
    use crate::models::agent::AgentType;
    use crate::models::loops::IssueConfig;
    use crate::web::event_bridge::EventEmitter;
    use std::process::Command as StdCommand;

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
            Some(&IssueConfig::default()),
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

        // A sibling that already SUCCEEDED with a real outcome — cancel must not
        // clobber it (C2). Its terminal status excludes it from the abandon bulk.
        let done = try_claim_iteration(
            &conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Refine,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "done-token".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        cas_iteration_status(&conn, done.id, IterationStatus::Queued, IterationStatus::Running)
            .await
            .unwrap();
        cas_iteration_status(&conn, done.id, IterationStatus::Running, IterationStatus::Succeeded)
            .await
            .unwrap();
        crate::db::service::loop_service::iteration::set_iteration_outcome(
            &conn,
            done.id,
            loop_iteration::IterationOutcome::Succeeded,
        )
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
        // D11: the cancelled in-flight iteration is recorded `abandoned`.
        assert_eq!(it.outcome, Some(loop_iteration::IterationOutcome::Abandoned));
        // C2: the succeeded sibling's real outcome is preserved, never overwritten.
        let done_after = loop_iteration::Entity::find_by_id(done.id)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            done_after.outcome,
            Some(loop_iteration::IterationOutcome::Succeeded),
            "cancel must not clobber a real outcome (C2)"
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

    #[tokio::test]
    async fn cancel_disconnects_live_agent() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // An in-flight running iteration bound to a conversation.
        let iter = try_claim_iteration(
            &conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Triage,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "tok".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        cas_iteration_status(&conn, iter.id, IterationStatus::Queued, IterationStatus::Running)
            .await
            .unwrap();
        let convo = 4242;
        loop_iteration::Entity::update_many()
            .col_expr(loop_iteration::Column::ConversationId, Expr::value(convo))
            .filter(loop_iteration::Column::Id.eq(iter.id))
            .exec(&conn)
            .await
            .unwrap();
        // A live agent connection whose session is bound to that conversation.
        engine
            .manager
            .insert_test_connection("agent-conn", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        engine
            .manager
            .get_state("agent-conn")
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(convo);
        assert!(engine
            .manager
            .find_connection_by_conversation_id(convo)
            .await
            .is_some());

        engine.cancel_issue(issue_id).await.unwrap();

        assert!(
            engine
                .manager
                .find_connection_by_conversation_id(convo)
                .await
                .is_none(),
            "the agent process connection is killed on cancel"
        );
    }

    // ── Blocked / budget escape hatches ─────────────────────────────────────

    #[tokio::test]
    async fn retry_rearms_blocked_task_and_resolves_cards() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // A blocked task carrying a failure signature + its no-progress card.
        let task = artifact::create_artifact(
            &conn,
            space_id,
            issue_id,
            ArtifactKind::Task,
            "T",
            ArtifactStatus::Blocked,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        loop_artifact::Entity::update_many()
            .col_expr(
                loop_artifact::Column::LastFailureSig,
                Expr::value("validation_failed:abc".to_string()),
            )
            .filter(loop_artifact::Column::Id.eq(task.id))
            .exec(&conn)
            .await
            .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();
        inbox::upsert_inbox(
            &conn,
            space_id,
            issue_id,
            None,
            InboxKind::Blocked,
            &format!("no_progress:{}", task.id),
            serde_json::json!({ "reason": "max_attempts" }),
        )
        .await
        .unwrap();

        engine.retry_issue(issue_id).await.unwrap();

        let t = loop_artifact::Entity::find_by_id(task.id)
            .one(&conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.status, ArtifactStatus::Pending, "blocked task re-armed");
        assert!(t.last_failure_sig.is_none(), "failure signature cleared");
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Running
        );
        assert!(
            inbox::list_inbox(&conn, space_id, Some(InboxStatus::Pending))
                .await
                .unwrap()
                .is_empty(),
            "blocking card resolved"
        );
        engine.stop_issue(issue_id).await;

        // Retrying an issue that is no longer blocked is a conflict.
        assert!(matches!(
            engine.retry_issue(issue_id).await,
            Err(LoopError::Conflict)
        ));
    }

    /// Helpers for the D13 oscillation-aware retry tests.
    async fn mk_blocked_task(
        conn: &sea_orm::DatabaseConnection,
        space_id: i32,
        issue_id: i32,
        title: &str,
    ) -> i32 {
        artifact::create_artifact(
            conn, space_id, issue_id, ArtifactKind::Task, title,
            ArtifactStatus::Blocked, ActorKind::Agent, None,
        )
        .await
        .unwrap()
        .id
    }
    async fn set_oscillating(conn: &sea_orm::DatabaseConnection, task: i32, count: i32) {
        loop_artifact::Entity::update_many()
            .col_expr(loop_artifact::Column::OscillationCount, Expr::value(count))
            .col_expr(
                loop_artifact::Column::RecentFailureSig,
                Expr::value("validation_failed:zzz".to_string()),
            )
            .filter(loop_artifact::Column::Id.eq(task))
            .exec(conn)
            .await
            .unwrap();
    }
    async fn card_pending(conn: &sea_orm::DatabaseConnection, issue_id: i32, subject: &str) -> bool {
        loop_inbox_item::Entity::find()
            .filter(loop_inbox_item::Column::IssueId.eq(issue_id))
            .filter(loop_inbox_item::Column::SubjectKey.eq(subject.to_string()))
            .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
            .one(conn)
            .await
            .unwrap()
            .is_some()
    }

    #[tokio::test]
    async fn retry_excludes_oscillating_tasks_and_keeps_their_card() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // One ordinary blocked task + one oscillating (count >= default limit 2).
        let ordinary = mk_blocked_task(&conn, space_id, issue_id, "ord").await;
        let osc = mk_blocked_task(&conn, space_id, issue_id, "osc").await;
        set_oscillating(&conn, osc, 2).await;
        for (t, prefix) in [(ordinary, "no_progress"), (osc, "oscillation")] {
            inbox::upsert_inbox(
                &conn, space_id, issue_id, None, InboxKind::Blocked,
                &format!("{prefix}:{t}"), serde_json::json!({ "reason": prefix }),
            )
            .await
            .unwrap();
        }
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        engine.retry_issue(issue_id).await.unwrap();
        engine.stop_issue(issue_id).await;

        let ord = loop_artifact::Entity::find_by_id(ordinary).one(&conn).await.unwrap().unwrap();
        assert_eq!(ord.status, ArtifactStatus::Pending, "ordinary task re-armed");
        assert_eq!(ord.attempt, 0, "attempt budget reset");
        let x = loop_artifact::Entity::find_by_id(osc).one(&conn).await.unwrap().unwrap();
        assert_eq!(x.status, ArtifactStatus::Blocked, "oscillating task NOT re-armed");
        assert_eq!(x.oscillation_count, 2, "oscillation columns preserved across retry");
        assert!(!card_pending(&conn, issue_id, &format!("no_progress:{ordinary}")).await);
        assert!(
            card_pending(&conn, issue_id, &format!("oscillation:{osc}")).await,
            "oscillation card survives a plain retry"
        );
    }

    #[tokio::test]
    async fn retry_all_oscillating_is_conflict() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let osc = mk_blocked_task(&conn, space_id, issue_id, "osc").await;
        set_oscillating(&conn, osc, 2).await;
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();
        assert!(matches!(
            engine.retry_issue(issue_id).await,
            Err(LoopError::Conflict)
        ));
        let x = loop_artifact::Entity::find_by_id(osc).one(&conn).await.unwrap().unwrap();
        assert_eq!(x.status, ArtifactStatus::Blocked, "issue untouched on conflict");
    }

    #[tokio::test]
    async fn retry_issue_level_block_redrives() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // No blocked task — an issue-level block (e.g. finalize dirty).
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            "finalize_dirty:issue", serde_json::json!({ "reason": "finalize_dirty" }),
        )
        .await
        .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        engine.retry_issue(issue_id).await.unwrap();
        engine.stop_issue(issue_id).await;

        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Running,
            "issue-level block still re-drives on retry"
        );
        assert!(!card_pending(&conn, issue_id, "finalize_dirty:issue").await);
    }

    #[tokio::test]
    async fn force_complete_only_for_empty_diff_cause() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let task = mk_blocked_task(&conn, space_id, issue_id, "t").await;
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        // Wrong cause (validation failure) → rejected even though the tree looks
        // clean. The guard reads the pending blocker CARD's `failure_sig` (mirroring
        // how the engine files blocks), NOT the artifact column.
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("validation_blocked:{task}"),
            serde_json::json!({ "failure_sig": "validation_failed:abc" }),
        )
        .await
        .unwrap();
        assert!(matches!(
            engine.force_complete_task(task).await,
            Err(LoopError::Conflict)
        ));
        assert_eq!(
            loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap().status,
            ArtifactStatus::Blocked
        );

        // Re-blocked for a genuine empty diff: resolve the stale validation card and
        // file the `no_progress` card the empty-diff path files (carrying
        // `failure_sig`). Now force-complete accepts it as a no-op.
        inbox::resolve_task_blocker_cards(
            &conn, issue_id, task, &["validation_blocked"], serde_json::json!({}),
        )
        .await
        .unwrap();
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("no_progress:{task}"),
            serde_json::json!({ "failure_sig": "empty_diff:implement", "reason": "max_attempts" }),
        )
        .await
        .unwrap();
        engine.force_complete_task(task).await.unwrap();
        engine.stop_issue(issue_id).await;

        let t = loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap();
        assert_eq!(t.status, ArtifactStatus::Done);
        assert_eq!(
            t.contribution_kind,
            crate::db::entities::loop_artifact::ContributionKind::NoOp
        );
        assert!(t.fan_in_commit.is_none(), "force-complete records a no-op (NULL commit)");
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Running
        );
        assert!(!card_pending(&conn, issue_id, &format!("no_progress:{task}")).await);
    }

    #[tokio::test]
    async fn force_complete_rejects_terminal_issue() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let task = mk_blocked_task(&conn, space_id, issue_id, "t").await;
        // A valid empty-diff blocker card so the cause guard passes — the rejection
        // must come from the terminal-issue check, not the cause guard.
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("no_progress:{task}"),
            serde_json::json!({ "failure_sig": "empty_diff:implement" }),
        )
        .await
        .unwrap();
        // Issue is cancelled → ensure_running_for_exit refuses.
        loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Cancelled.to_value()),
            )
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            engine.force_complete_task(task).await,
            Err(LoopError::Conflict)
        ));
        assert_eq!(
            loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap().status,
            ArtifactStatus::Blocked,
            "task untouched when the issue can't be anchored"
        );
    }

    #[tokio::test]
    async fn override_oscillation_rearms_and_clears_cards() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let task = mk_blocked_task(&conn, space_id, issue_id, "t").await;
        set_oscillating(&conn, task, 2).await;
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("oscillation:{task}"), serde_json::json!({ "reason": "oscillation" }),
        )
        .await
        .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        engine.override_oscillation(task).await.unwrap();
        engine.stop_issue(issue_id).await;

        let t = loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap();
        assert_eq!(t.status, ArtifactStatus::Pending, "task re-armed");
        assert_eq!(t.attempt, 0);
        assert_eq!(t.oscillation_count, 0, "oscillation epoch cleared");
        assert!(t.recent_failure_sig.is_none());
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Running
        );
        assert!(
            !card_pending(&conn, issue_id, &format!("oscillation:{task}")).await,
            "oscillation card resolved by the override"
        );
    }

    /// D17 precondition (Codex r2): override is for breaker-promoted tasks ONLY. A
    /// blocked task with no pending `oscillation:` card (here an ordinary no_progress
    /// block) is rejected, so the endpoint can never be a generic blocked-task reset.
    #[tokio::test]
    async fn override_rejects_non_oscillating_blocked_task() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let task = mk_blocked_task(&conn, space_id, issue_id, "t").await;
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("no_progress:{task}"),
            serde_json::json!({ "failure_sig": "empty_diff:implement" }),
        )
        .await
        .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        assert!(matches!(
            engine.override_oscillation(task).await,
            Err(LoopError::Conflict)
        ));
        assert_eq!(
            loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap().status,
            ArtifactStatus::Blocked,
            "task untouched without a pending oscillation card"
        );
    }

    /// Codex r2: the force-complete cause guard is re-validated inside the txn. If a
    /// concurrent retry resolved the blocker card (re-arming the task), the pending
    /// card vanishes — force-complete must reject rather than no-op a task whose block
    /// is no longer a live empty diff. (A true mid-txn race isn't deterministically
    /// injectable in this harness; this exercises the resolved-card guarded state the
    /// in-txn re-read enforces.)
    #[tokio::test]
    async fn force_complete_rejects_when_blocker_card_resolved() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let task = mk_blocked_task(&conn, space_id, issue_id, "t").await;
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();
        inbox::upsert_inbox(
            &conn, space_id, issue_id, None, InboxKind::Blocked,
            &format!("no_progress:{task}"),
            serde_json::json!({ "failure_sig": "empty_diff:implement" }),
        )
        .await
        .unwrap();
        // A concurrent retry would resolve the blocker card while re-arming the task.
        inbox::resolve_task_blocker_cards(
            &conn, issue_id, task, &["no_progress"], serde_json::json!({}),
        )
        .await
        .unwrap();

        assert!(matches!(
            engine.force_complete_task(task).await,
            Err(LoopError::Conflict)
        ));
        assert_eq!(
            loop_artifact::Entity::find_by_id(task).one(&conn).await.unwrap().unwrap().status,
            ArtifactStatus::Blocked,
            "a vanished blocker card blocks force-complete"
        );
    }

    /// Codex r3: an exit action entered with a STALE `running` issue model must still
    /// re-anchor from the LIVE DB state. A concurrent driver re-park could have flipped
    /// the row `running → blocked` after the caller read it; the old helper trusted the
    /// model and returned Ok without writing, which would strand an all-done issue
    /// `blocked` with no actionable card. The helper now re-anchors authoritatively.
    #[tokio::test]
    async fn ensure_running_for_exit_reanchors_stale_running_model() {
        let (_engine, conn, _space_id, issue_id) = setup().await;
        // A model captured while the issue was running ...
        let stale = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(stale.status, IssueStatus::Running);
        // ... while the live row was re-parked to blocked by a driver.
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();

        ensure_running_for_exit(&conn, &stale).await.unwrap();
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Running,
            "re-anchored from the live blocked row, not the stale running model"
        );

        // A terminal issue is refused even when the stale model still says running.
        loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::Status,
                Expr::value(IssueStatus::Cancelled.to_value()),
            )
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            ensure_running_for_exit(&conn, &stale).await,
            Err(LoopError::Conflict)
        ));
    }

    #[tokio::test]
    async fn add_budget_tops_up_and_resumes() {
        let (engine, conn, space_id, issue_id) = setup().await;
        // A budget-paused issue: budget set, paused(budget), card filed.
        loop_issue::Entity::update_many()
            .col_expr(loop_issue::Column::TokenBudget, Expr::value(1000_i64))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&conn)
            .await
            .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Paused)
            .await
            .unwrap();
        loop_issue::Entity::update_many()
            .col_expr(
                loop_issue::Column::PauseReason,
                Expr::value(PauseReason::Budget.to_value()),
            )
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&conn)
            .await
            .unwrap();
        inbox::upsert_inbox(
            &conn,
            space_id,
            issue_id,
            None,
            InboxKind::BudgetExhausted,
            &format!("budget:{issue_id}"),
            serde_json::json!({ "token_used": 1200, "token_budget": 1000 }),
        )
        .await
        .unwrap();

        engine.add_budget(issue_id, 5000).await.unwrap();

        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.token_budget, Some(6000), "budget topped up");
        assert_eq!(issue.status, IssueStatus::Running, "issue resumed");
        assert_eq!(issue.pause_reason, None, "pause reason cleared");
        assert!(
            inbox::list_inbox(&conn, space_id, Some(InboxStatus::Pending))
                .await
                .unwrap()
                .is_empty(),
            "budget card resolved"
        );
        engine.stop_issue(issue_id).await;

        // Adding budget to a running (non-paused) issue is a conflict.
        assert!(matches!(
            engine.add_budget(issue_id, 1000).await,
            Err(LoopError::Conflict)
        ));
    }

    // ── Merge gate (real git repo) ──────────────────────────────────────────

    fn git(dir: &Path, args: &[&str]) {
        let st = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "tester"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    /// Engine + real git repo + an issue triggered (worktree created, running)
    /// carrying one loop commit and a produced `result` artifact — i.e. a fully
    /// finalized issue sitting at the merge gate.
    async fn setup_repo() -> (
        Arc<LoopEngine>,
        sea_orm::DatabaseConnection,
        tempfile::TempDir,
        tempfile::TempDir,
        i32,
        i32,
    ) {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let data = tempfile::tempdir().unwrap();
        let db = fresh_disk_db(data.path()).await;
        let conn = db.conn.clone();
        let folder_id = seed_folder(&db, &repo.path().to_string_lossy()).await;
        let space = space::create_space(&conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            data.path().to_path_buf(),
            EventEmitter::Noop,
        );
        // Trigger: create the worktree (records the base), flip running.
        let ctx = worktree::ensure_worktree(&conn, data.path(), issue.row.id)
            .await
            .unwrap();
        cas_issue_status(&conn, issue.row.id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        // One loop commit so the landing has content.
        std::fs::write(ctx.worktree_path.join("feature.txt"), "work\n").unwrap();
        worktree::checkpoint(&ctx.worktree_path, "loop: feature")
            .await
            .unwrap()
            .expect("committed");
        // Finalize produced the result artifact.
        let result = artifact::create_artifact(
            &conn,
            space.id,
            issue.row.id,
            ArtifactKind::Result,
            "result",
            ArtifactStatus::Done,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        // Integration verified: the merge gate (D6) requires a recorded
        // `gate_decision(result, finalize) == Pass`, so a finalized issue at the
        // merge gate must carry it.
        crate::db::service::loop_service::gate_decision::record_decision(
            &conn,
            space.id,
            issue.row.id,
            result.id,
            crate::loop_engine::gates::FINALIZE_GATE_STAGE,
            result.attempt,
            &[],
            &[],
            "{}",
            crate::db::entities::loop_gate_decision::GateOutcome::Pass,
        )
        .await
        .unwrap();
        (engine, conn, repo, data, issue.row.id, ctx.worktree_folder_id)
    }

    #[tokio::test]
    async fn merge_issue_success_closes_issue_and_removes_worktree() {
        let (engine, conn, repo, _data, issue_id, folder_id) = setup_repo().await;
        let worktree_path = PathBuf::from(
            folder_service::get_folder_by_id(&conn, folder_id)
                .await
                .unwrap()
                .unwrap()
                .path,
        );

        engine.merge_issue(issue_id).await.unwrap();

        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Done);
        assert!(issue.ended_at.is_some());
        // Worktree folder soft-deleted + directory gone.
        assert!(folder_service::get_folder_by_id(&conn, folder_id)
            .await
            .unwrap()
            .is_none());
        assert!(!worktree_path.exists());
        // The loop work landed on the base branch.
        assert!(repo.path().join("feature.txt").exists());
    }

    #[tokio::test]
    async fn merge_issue_dirty_base_blocks_and_errors() {
        let (engine, conn, repo, _data, issue_id, _folder_id) = setup_repo().await;
        // Modify a TRACKED file in the base repo (untracked files no longer block).
        std::fs::write(repo.path().join("README.md"), "locally modified\n").unwrap();

        // The fault surfaces as an error — not a silent "Ok" success.
        let err = engine.merge_issue(issue_id).await.unwrap_err();
        assert!(matches!(err, LoopError::MergeFailed(_)));

        // The issue is blocked + carries a durable card so the fault is visible
        // (also covers the auto-merge path, which only logs the error).
        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Blocked);
        let cards = inbox::list_inbox(&conn, issue.space_id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert!(cards.iter().any(|c| c.kind == InboxKind::Blocked
            && c.subject_key == format!("merge_blocked:{issue_id}")));
        assert!(!repo.path().join("feature.txt").exists(), "nothing landed");
    }

    #[tokio::test]
    async fn merge_issue_conflict_blocks_with_inbox_and_errors() {
        let (engine, conn, repo, _data, issue_id, _folder_id) = setup_repo().await;
        // Advance the base branch with a CONFLICTING change to feature.txt (the
        // loop branch added feature.txt too), so integrating the base conflicts.
        std::fs::write(repo.path().join("feature.txt"), "base conflicting\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "base feature"]);

        // The fault surfaces as an error (never a silent success)...
        let err = engine.merge_issue(issue_id).await.unwrap_err();
        assert!(matches!(err, LoopError::MergeFailed(_)));

        // ...AND a branch/integration fault blocks the issue + files a card so it
        // is visible (also covers the auto-merge path, which only logs the error).
        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Blocked);
        let cards = inbox::list_inbox(&conn, issue.space_id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert!(cards.iter().any(|c| c.kind == InboxKind::Blocked
            && c.subject_key == format!("merge_blocked:{issue_id}")));
        // The loop's work never landed: the base still holds its own version.
        assert_eq!(
            std::fs::read_to_string(repo.path().join("feature.txt")).unwrap(),
            "base conflicting\n"
        );
    }

    #[tokio::test]
    async fn merge_issue_without_result_not_mergeable() {
        let (engine, conn, _repo, _data, issue_id, _folder_id) = setup_repo().await;
        // Drop the result artifact to simulate "finalize not done".
        loop_artifact::Entity::delete_many()
            .filter(loop_artifact::Column::IssueId.eq(issue_id))
            .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Result))
            .exec(&conn)
            .await
            .unwrap();
        assert!(matches!(
            engine.merge_issue(issue_id).await,
            Err(LoopError::NotMergeable)
        ));
    }

    #[tokio::test]
    async fn merge_issue_second_call_after_cleanup_is_idempotent_ok() {
        let (engine, conn, _repo, _data, issue_id, folder_id) = setup_repo().await;
        let worktree_path = PathBuf::from(
            folder_service::get_folder_by_id(&conn, folder_id)
                .await
                .unwrap()
                .unwrap()
                .path,
        );

        engine.merge_issue(issue_id).await.unwrap();
        // First merge landed and tore the worktree down.
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Done
        );
        assert!(!worktree_path.exists(), "first merge removed the worktree");
        assert!(folder_service::get_folder_by_id(&conn, folder_id)
            .await
            .unwrap()
            .is_none());

        // Second merge, with the worktree already removed, is a no-op SUCCESS —
        // not LoopError::Conflict ("state changed concurrently; retry"). The
        // idempotent branch returns at the post-lock `done` re-read before it ever
        // touches the absent worktree.
        engine.merge_issue(issue_id).await.unwrap();
        assert_eq!(
            issue::get_issue(&conn, issue_id).await.unwrap().unwrap().status,
            IssueStatus::Done
        );
    }

    #[tokio::test]
    async fn merge_issue_blocked_is_not_mergeable() {
        let (engine, conn, _repo, _data, issue_id, _folder_id) = setup_repo().await;
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Blocked)
            .await
            .unwrap();
        assert!(matches!(
            engine.merge_issue(issue_id).await,
            Err(LoopError::NotMergeable)
        ));
    }

    // ── Design approval gate ────────────────────────────────────────────────

    /// Mint an `awaiting_approval` design + its inbox card on a running issue.
    async fn seed_awaiting_design(conn: &sea_orm::DatabaseConnection, space_id: i32, issue_id: i32) -> i32 {
        let d = artifact::create_artifact(
            conn,
            space_id,
            issue_id,
            ArtifactKind::Design,
            "D1",
            ArtifactStatus::AwaitingApproval,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        artifact::add_revision(conn, d.id, "design body", ActorKind::Agent, None)
            .await
            .unwrap();
        inbox::upsert_inbox(
            conn,
            space_id,
            issue_id,
            None,
            InboxKind::Approval,
            &format!("design:{issue_id}"),
            serde_json::json!({ "gate": "design" }),
        )
        .await
        .unwrap();
        d.id
    }

    #[tokio::test]
    async fn approve_design_marks_done_and_resolves_card() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let design_id = seed_awaiting_design(&conn, space_id, issue_id).await;

        engine.approve_design(issue_id).await.unwrap();

        let detail = artifact::get_artifact_detail(&conn, design_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.row.status, ArtifactStatus::Done);
        let pending = inbox::list_inbox(&conn, space_id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert!(!pending
            .iter()
            .any(|c| c.subject_key == format!("design:{issue_id}")));
        // Nothing awaiting now → a second approve conflicts.
        assert!(matches!(
            engine.approve_design(issue_id).await,
            Err(LoopError::Conflict)
        ));
    }

    #[tokio::test]
    async fn reject_design_supersedes_and_records_comment() {
        let (engine, conn, space_id, issue_id) = setup().await;
        let design_id = seed_awaiting_design(&conn, space_id, issue_id).await;

        engine
            .reject_design(issue_id, Some("needs more detail".into()))
            .await
            .unwrap();

        let detail = artifact::get_artifact_detail(&conn, design_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.row.status, ArtifactStatus::Superseded);
        assert!(detail.revisions.iter().any(|r| r.actor_kind == ActorKind::Human
            && r.content.contains("needs more detail")));
        let pending = inbox::list_inbox(&conn, space_id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert!(!pending
            .iter()
            .any(|c| c.subject_key == format!("design:{issue_id}")));
    }

    // ---- reflect orchestration (P4.4) ----

    /// Claim a reflect iteration and drive it to terminal `Failed` — a spent
    /// attempt with no artifact (what the exhaustion counter sees).
    async fn fail_reflect(
        conn: &sea_orm::DatabaseConnection,
        space_id: i32,
        issue_id: i32,
        token: &str,
    ) {
        let it = try_claim_iteration(
            conn,
            IterationClaim {
                space_id,
                issue_id,
                stage: Stage::Reflect,
                target_artifact_id: None,
                slot_no: None,
                capability_token: token.into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        cas_iteration_status(conn, it.id, IterationStatus::Queued, IterationStatus::Running)
            .await
            .unwrap();
        cas_iteration_status(conn, it.id, IterationStatus::Running, IterationStatus::Failed)
            .await
            .unwrap();
    }

    async fn count_reflect_iters(conn: &sea_orm::DatabaseConnection, issue_id: i32) -> usize {
        loop_iteration::Entity::find()
            .filter(loop_iteration::Column::IssueId.eq(issue_id))
            .filter(loop_iteration::Column::Stage.eq(Stage::Reflect))
            .all(conn)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn reflect_dispatch_is_noop_when_reflection_artifact_exists() {
        let (engine, conn, space_id, issue_id) = setup().await;
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Done)
            .await
            .unwrap();
        // The durable anchor (D12): a reflection already exists for the issue.
        artifact::create_artifact(
            &conn,
            space_id,
            issue_id,
            ArtifactKind::Reflection,
            "Retro",
            ArtifactStatus::Done,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let issue = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();

        engine.dispatch_reflect_best_effort(&issue).await;

        assert_eq!(
            count_reflect_iters(&conn, issue_id).await,
            0,
            "anchor present → no dispatch"
        );
    }

    #[tokio::test]
    async fn reflect_dispatch_files_card_at_max_attempts() {
        let db = fresh_in_memory_db().await;
        let conn = db.conn.clone();
        let folder_id = seed_folder(&db, "/tmp/loop-reflect-exhaust").await;
        let space = space::create_space(&conn, "S", folder_id).await.unwrap();
        let cfg = IssueConfig {
            max_attempts: 1,
            ..IssueConfig::default()
        };
        let issue = issue::create_issue(&conn, space.id, "I", "b", IssuePriority::Medium, Some(&cfg))
            .await
            .unwrap();
        let issue_id = issue.row.id;
        cas_issue_status(&conn, issue_id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Done)
            .await
            .unwrap();
        // One terminal (failed) reflect attempt — at max_attempts (1), no artifact.
        fail_reflect(&conn, space.id, issue_id, "reflect-fail-1").await;
        let engine = LoopEngine::new(
            db,
            ConnectionManager::new(),
            std::path::PathBuf::from("/tmp/loop-reflect-exhaust-data"),
            EventEmitter::Noop,
        );
        let model = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();

        engine.dispatch_reflect_best_effort(&model).await;

        // A ReflectionFailed card was filed; no new reflect iteration claimed.
        let pending = inbox::list_inbox(&conn, space.id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert!(pending.iter().any(|c| c.kind == InboxKind::ReflectionFailed));
        assert_eq!(
            count_reflect_iters(&conn, issue_id).await,
            1,
            "exhausted → no further dispatch"
        );
    }

    #[tokio::test]
    async fn reflect_settle_is_done_safe_with_exhausted_budget() {
        let db = fresh_in_memory_db().await;
        let conn = db.conn.clone();
        let folder_id = seed_folder(&db, "/tmp/loop-reflect-budget").await;
        let space = space::create_space(&conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &conn,
            space.id,
            "I",
            "b",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let issue_id = issue.row.id;
        cas_issue_status(&conn, issue_id, IssueStatus::Pending, IssueStatus::Running)
            .await
            .unwrap();
        cas_issue_status(&conn, issue_id, IssueStatus::Running, IssueStatus::Done)
            .await
            .unwrap();
        // An already-exceeded token budget on the Done issue.
        loop_issue::Entity::update_many()
            .col_expr(loop_issue::Column::TokenBudget, Expr::value(100i64))
            .col_expr(loop_issue::Column::TokenUsed, Expr::value(200i64))
            .filter(loop_issue::Column::Id.eq(issue_id))
            .exec(&conn)
            .await
            .unwrap();
        // A running reflect iteration (target = None).
        let it = try_claim_iteration(
            &conn,
            IterationClaim {
                space_id: space.id,
                issue_id,
                stage: Stage::Reflect,
                target_artifact_id: None,
                slot_no: None,
                capability_token: "reflect-budget-tok".into(),
                attempt: 0,
            },
        )
        .await
        .unwrap()
        .unwrap();
        cas_iteration_status(&conn, it.id, IterationStatus::Queued, IterationStatus::Running)
            .await
            .unwrap();

        // Settle: no-progress breaker skipped (target None); budget CAS
        // Running→Paused misses on a Done issue — Ok, no status change.
        crate::loop_engine::dispatch::settle_iteration(&db, &EventEmitter::Noop, it.id)
            .await
            .unwrap();

        let after = issue::get_issue(&conn, issue_id).await.unwrap().unwrap();
        assert_eq!(
            after.status,
            IssueStatus::Done,
            "reflect settle never disturbs a Done issue"
        );
    }
}
