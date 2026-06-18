//! Parallel result-stage fan-in (spec §4.4): atomically integrate a parallel
//! issue's frozen per-task commits onto its issue branch, then synthesize the
//! result.
//!
//! The shape is a **deferred, recoverable, atomic** integration:
//! 1. Claim a write-once session lock — the versioned `fan_in_manifest`
//!    (`{v, issue_base_oid, ordered:[{task_id, sha}]}`), distinct from the
//!    in-flight-agent lease. `ordered` freezes the topological merge order so a
//!    resume replays it rather than recomputing from mutable DB state.
//! 2. Merge each frozen task commit into a temp `integrate` worktree/branch
//!    ([`worktree::fan_in_tasks`]) — resumable (already-merged commits skip),
//!    conflict-aware (a conflict is handed to a result-stage agent that resolves
//!    it and `git commit`s).
//! 3. CAS-land the integrate tip onto the issue branch
//!    ([`worktree::cas_advance_branch`]) — atomic w.r.t. the issue branch, so a
//!    crash mid-fan-in leaves the issue branch untouched (the integrate branch is
//!    discardable). Only AFTER landing is the result artifact synthesized, so a
//!    failed land never strands a result row blocking retry.
//!
//! Crash recovery (every step is re-entrant):
//! - **Already-landed detection** runs before any re-merge: if the issue branch
//!   already contains every frozen commit (a prior land succeeded but we crashed
//!   before finishing), we repair-and-finish idempotently WITHOUT re-validating —
//!   so flaky re-validation can never block work that already landed.
//! - **Conflict-resolver liveness** is tracked by `fan_in_resolver_tip`: a
//!   `MERGE_HEAD` with no resolver recorded for that tip is a crash-before-dispatch
//!   (re-dispatch); a `MERGE_HEAD` at the recorded tip is a resolver that ran and
//!   left it unresolved (block).
//!
//! Serial issues never enter here — they keep the agent-submitted finalize path.

use std::path::Path;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_artifact_revision::{self, ActorKind};
use crate::db::entities::loop_inbox_item::InboxKind;
use crate::db::entities::loop_issue::{self, IssueStatus};
use crate::db::entities::loop_iteration::{self, IterationStatus, Stage};
use crate::db::entities::loop_link::{self, LinkKind};
use crate::db::service::{folder_service, loop_service};
use crate::db::AppDatabase;
use crate::models::loops::{IssueConfig, LoopArtifactRow, LoopDagView};
use crate::web::event_bridge::EventEmitter;

use crate::loop_engine::dispatch::{
    dispatch_iteration, emit_changed, DispatchInput, LoopAgentSpawner,
};
use crate::loop_engine::driver::resolve_agent_spec;
use crate::loop_engine::error::LoopError;
use crate::loop_engine::gates::StepOutcome;
use crate::loop_engine::transitions::{
    cas_issue_status, clear_fan_in, set_fan_in_resolver_tip, try_claim_fan_in,
};
use crate::loop_engine::worktree::{self, FanInOutcome};

/// One frozen task commit in the fan-in manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FanInEntry {
    task_id: i32,
    sha: String,
}

/// Versioned, write-once fan-in session manifest. `ordered` is the topological
/// merge order frozen at claim time — a resume replays it verbatim, never
/// recomputing from (mutable) DB state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FanInManifest {
    v: u32,
    /// The issue branch tip at claim time — the integrate branch's base AND the
    /// CAS `expected_old` for the landing.
    issue_base_oid: String,
    /// D12: stable epoch anchor — the active task set at claim time, sorted. Resume
    /// validates "fan-in set complete" against THIS exact set, never live DB state.
    /// `#[serde(default)]` tolerates a pre-D12 (v1) manifest that lacked it.
    #[serde(default)]
    active_task_ids: Vec<i32>,
    /// Delta tasks (carry a frozen commit) — the topological merge order.
    ordered: Vec<FanInEntry>,
    /// D12: no-op tasks (agent-declared satisfied; no commit) — recorded for
    /// provenance, skipped by the merge. `ordered ∪ skipped == active_task_ids`.
    #[serde(default)]
    skipped_no_op_task_ids: Vec<i32>,
}

impl FanInManifest {
    fn ordered_pairs(&self) -> Vec<(i32, String)> {
        self.ordered
            .iter()
            .map(|e| (e.task_id, e.sha.clone()))
            .collect()
    }

    /// All member task ids — delta (merged) ∪ no-op (skipped). The provenance set
    /// the result capstone links `ResultsFrom`, so no-op tasks are not dropped from
    /// the lineage (D12).
    fn all_member_task_ids(&self) -> Vec<i32> {
        self.ordered
            .iter()
            .map(|e| e.task_id)
            .chain(self.skipped_no_op_task_ids.iter().copied())
            .collect()
    }
}

fn parse_manifest(json: &str) -> Result<FanInManifest, LoopError> {
    let m: FanInManifest = serde_json::from_str(json)
        .map_err(|e| LoopError::InvalidInput(format!("fan-in manifest decode: {e}")))?;
    // D12 (Codex r1): validate the v2 partition on resume too, not only at build —
    // `ordered ∪ skipped_no_op_task_ids == active_task_ids`. A v1 manifest (no
    // `active_task_ids`, pre-D12) skips the check. Guards against a corrupted /
    // hand-edited manifest stranding a task on replay.
    if !m.active_task_ids.is_empty() {
        let mut covered: Vec<i32> = m
            .ordered
            .iter()
            .map(|e| e.task_id)
            .chain(m.skipped_no_op_task_ids.iter().copied())
            .collect();
        covered.sort_unstable();
        let mut active = m.active_task_ids.clone();
        active.sort_unstable();
        if covered != active {
            return Err(LoopError::Git(
                "fan-in manifest partition does not cover the active task set (resume)".into(),
            ));
        }
    }
    Ok(m)
}

/// The all-no_op fast-path decision (D12, Codex r2). An all-no_op manifest
/// (`ordered` empty) means no task contributed a commit, so the integration MUST be
/// exactly the issue base. This is decided BEFORE the landed-recovery check, whose
/// `all_frozen_ancestors` predicate is vacuously true for an empty `ordered` and
/// would otherwise mistake any moved tip for "already landed".
#[derive(Debug, PartialEq, Eq)]
enum NoOpGate {
    /// Empty manifest and the branch is at base → finish (idempotent).
    FinishAtBase,
    /// Empty manifest but the branch advanced past base → anomalous; block, never
    /// synthesize a result against an unreviewed tip.
    BlockMovedTip,
    /// Has frozen commits → not an all-no_op session; fall through to normal flow.
    NotAllNoOp,
}

fn no_op_gate(manifest: &FanInManifest, issue_tip: &str) -> NoOpGate {
    if !manifest.ordered.is_empty() {
        return NoOpGate::NotAllNoOp;
    }
    if issue_tip == manifest.issue_base_oid {
        NoOpGate::FinishAtBase
    } else {
        NoOpGate::BlockMovedTip
    }
}

/// Drive a parallel issue's result-stage fan-in for one tick. Returns
/// [`StepOutcome`] like the gates: `Dispatched` (a conflict resolver is in
/// flight), `Advanced` (durable progress — landed / blocked / restarted; re-tick),
/// or `Idle` (waiting on in-flight work). Called from [`super::gates::run_finalize`]
/// only when the issue is `parallel` and its result does not yet exist.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_parallel_finalize(
    db: &AppDatabase,
    data_dir: &Path,
    spawner: &dyn LoopAgentSpawner,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    dag: &LoopDagView,
    config: &IssueConfig,
    issue_worktree_folder_id: i32,
) -> Result<StepOutcome, LoopError> {
    let conn = &db.conn;

    // Wait while any iteration is in flight (a conflict resolver, or stray work)
    // — never reset/re-merge under a live agent.
    if issue_has_inflight(db, issue.id).await? {
        return Ok(StepOutcome::Idle);
    }

    let space = loop_service::space::get_space(conn, issue.space_id)
        .await?
        .ok_or_else(|| LoopError::NotFound(format!("space {}", issue.space_id)))?;
    let repo = folder_service::get_folder_by_id(conn, space.folder_id)
        .await?
        .ok_or(LoopError::Detached)?;
    let repo_path = Path::new(&repo.path);
    let issue_branch = format!("loop/{}/issue-{}", issue.space_id, issue.seq_no);

    // Claim or adopt the fan-in manifest (write-once session lock). Keep the exact
    // stored JSON alongside the parsed form — `clear_fan_in` CAS-guards on it.
    let (manifest, manifest_json) = match &issue.fan_in_manifest {
        Some(j) => (parse_manifest(j)?, j.clone()),
        None => {
            let m = build_manifest(db, dag, issue_worktree_folder_id).await?;
            let json = serde_json::to_string(&m)
                .map_err(|e| LoopError::InvalidInput(format!("fan-in manifest encode: {e}")))?;
            if try_claim_fan_in(conn, issue.id, &json).await? {
                (m, json)
            } else {
                let fresh = loop_issue::Entity::find_by_id(issue.id)
                    .one(conn)
                    .await?
                    .and_then(|i| i.fan_in_manifest)
                    .ok_or_else(|| {
                        LoopError::Git("fan-in manifest vanished after a lost claim".into())
                    })?;
                (parse_manifest(&fresh)?, fresh)
            }
        }
    };
    // Provenance set = ALL members (delta + no-op), so the result capstone links
    // every contributing task, not just the merged ones (D12).
    let task_ids = manifest.all_member_task_ids();

    // Ensure the integrate worktree (attach-first preserves in-progress merges).
    let integrate = worktree::ensure_integrate_worktree(
        conn,
        data_dir,
        issue.id,
        &manifest.issue_base_oid,
    )
    .await?;
    let integrate_path = integrate.worktree_path.clone();

    let issue_tip = worktree::resolve_oid(repo_path, &format!("refs/heads/{issue_branch}")).await?;

    // [D12] All-no_op fast path, decided BEFORE the landed-recovery check below:
    // `all_frozen_ancestors` is vacuously true for an empty `ordered`, so a moved tip
    // would otherwise be mistaken for "already landed" and synthesize a result against
    // an unreviewed tip (Codex r2). `finish_landed` is idempotent and the capstone
    // still links every no-op member for provenance.
    match no_op_gate(&manifest, &issue_tip) {
        NoOpGate::FinishAtBase => {
            return finish_landed(
                db,
                emitter,
                issue,
                &task_ids,
                &manifest_json,
                repo_path,
                &integrate_path,
                issue_worktree_folder_id,
            )
            .await;
        }
        NoOpGate::BlockMovedTip => {
            return block_fan_in(
                db,
                emitter,
                issue,
                "fan_in_all_no_op_unexpected_tip",
                "every task declared a no-op but the issue branch advanced past its base",
            )
            .await;
        }
        NoOpGate::NotAllNoOp => {}
    }

    // [recovery] Already landed? A prior land advanced the issue branch but we
    // crashed before synthesizing the result / clearing the session. The issue
    // branch then contains every frozen commit (and `ordered` is non-empty, so this
    // check is not vacuous). Repair-and-finish idempotently — crucially WITHOUT
    // re-running the merge/validation, so flaky re-validation can never block work
    // that already landed.
    if issue_tip != manifest.issue_base_oid
        && all_frozen_ancestors(repo_path, &issue_tip, &manifest).await?
    {
        return finish_landed(
            db,
            emitter,
            issue,
            &task_ids,
            &manifest_json,
            repo_path,
            &integrate_path,
            issue_worktree_folder_id,
        )
        .await;
    }

    // [recovery] A merge left mid-flight (MERGE_HEAD) with NO resolver in flight
    // (we passed the in-flight gate). Distinguish the two ways that happens:
    //   - the integrate tip matches `fan_in_resolver_tip` → a resolver already ran
    //     from this exact tip and left the merge unresolved → structural block;
    //   - otherwise → we crashed after `fan_in_tasks` left MERGE_HEAD but before a
    //     resolver was dispatched (or the tip advanced past an earlier resolved
    //     conflict) → dispatch a resolver now.
    if worktree::integrate_in_progress(&integrate_path).await {
        let cur = worktree::head_commit(&integrate_path).await?;
        if issue.fan_in_resolver_tip.as_deref() == Some(cur.as_str()) {
            return block_fan_in(
                db,
                emitter,
                issue,
                "fan_in_conflict_unresolved",
                "a fan-in merge conflict was left unresolved by the result-stage agent",
            )
            .await;
        }
        return dispatch_resolver_at(
            db,
            data_dir,
            spawner,
            emitter,
            issue,
            config,
            integrate.worktree_folder_id,
            &cur,
        )
        .await;
    }
    // Clear any stray uncommitted state (committed merges are preserved by HEAD).
    worktree::reset_to_head(&integrate_path).await?;

    match worktree::fan_in_tasks(
        &integrate_path,
        &manifest.ordered_pairs(),
        &config.validation_commands,
        config.iteration_timeout_secs,
    )
    .await?
    {
        FanInOutcome::Conflict { .. } => {
            // Hand the in-progress merge to a result-stage agent that resolves it
            // and `git commit`s (working in the integrate worktree). Record the tip
            // we dispatch from so a resolver that fails to resolve is detected on
            // re-entry (above) rather than re-dispatched forever.
            let cur = worktree::head_commit(&integrate_path).await?;
            dispatch_resolver_at(
                db,
                data_dir,
                spawner,
                emitter,
                issue,
                config,
                integrate.worktree_folder_id,
                &cur,
            )
            .await
        }
        FanInOutcome::RevalidationFailed { .. } => {
            block_fan_in(
                db,
                emitter,
                issue,
                "fan_in_revalidation_failed",
                "the integrated tree failed re-validation; the task combination broke",
            )
            .await
        }
        FanInOutcome::Integrated { tip } => {
            land_integration(
                db,
                emitter,
                issue,
                &task_ids,
                &manifest_json,
                repo_path,
                &integrate_path,
                &issue_branch,
                issue_worktree_folder_id,
                &manifest.issue_base_oid,
                &tip,
            )
            .await
        }
    }
}

/// CAS-land the integrate tip onto the issue branch, then finish (sync worktree,
/// synthesize result, tear down). A genuine lost CAS (the issue branch moved)
/// discards the integration and restarts; a hard `update-ref` error propagates
/// ([`worktree::cas_advance_branch`] disambiguates the two).
#[allow(clippy::too_many_arguments)]
async fn land_integration(
    db: &AppDatabase,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    task_ids: &[i32],
    manifest_json: &str,
    repo_path: &Path,
    integrate_path: &Path,
    issue_branch: &str,
    issue_worktree_folder_id: i32,
    base_oid: &str,
    tip: &str,
) -> Result<StepOutcome, LoopError> {
    let conn = &db.conn;

    if !worktree::cas_advance_branch(repo_path, issue_branch, tip, base_oid).await? {
        // Lost CAS (the issue branch moved under us) → discard the integration,
        // clear the session, and restart fresh next tick.
        cleanup_integrate(issue, repo_path, integrate_path).await;
        clear_fan_in(conn, issue.id, manifest_json).await?;
        emit_changed(emitter, issue.space_id, issue.id, issue.id, "iteration");
        return Ok(StepOutcome::Advanced);
    }

    finish_landed(
        db,
        emitter,
        issue,
        task_ids,
        manifest_json,
        repo_path,
        integrate_path,
        issue_worktree_folder_id,
    )
    .await
}

/// Finish a landed fan-in: sync the issue worktree to the new tip, synthesize the
/// result (AFTER the worktree is clean), clear the session, tear down the integrate
/// worktree. Re-entrant — each step is idempotent, so a crash anywhere replays via
/// the already-landed detection.
#[allow(clippy::too_many_arguments)]
async fn finish_landed(
    db: &AppDatabase,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    task_ids: &[i32],
    manifest_json: &str,
    repo_path: &Path,
    integrate_path: &Path,
    issue_worktree_folder_id: i32,
) -> Result<StepOutcome, LoopError> {
    let conn = &db.conn;

    // Sync the issue worktree to the landed tip FIRST. `update-ref` moved the branch
    // ref but not the worktree's tree; that tree is now stale (a reverse diff vs
    // HEAD). It MUST be reset before the result exists — otherwise the shared
    // finalize tail (run once `has_result`) would `checkpoint` the stale tree,
    // committing a reverse diff onto the issue branch. We have not yet created the
    // result, so a failure here simply re-ticks (already-landed detection retries)
    // and never strands a half-finished issue with a dirty tree.
    if let Some(folder) = folder_service::get_folder_by_id(conn, issue_worktree_folder_id).await? {
        let p = Path::new(&folder.path);
        if p.exists() {
            worktree::reset_to_head(p).await?;
        }
    }

    // Produce the result AFTER the worktree is clean and the branch has landed — a
    // failed land never strands a result row, and the result is the durable
    // done-marker, so it is created before the session lock is cleared.
    create_result_artifact(conn, issue, task_ids).await?;

    clear_fan_in(conn, issue.id, manifest_json).await?;
    cleanup_integrate(issue, repo_path, integrate_path).await;
    emit_changed(emitter, issue.space_id, issue.id, issue.id, "iteration");
    // Result now exists → re-tick: run_finalize's shared tail opens the merge gate.
    Ok(StepOutcome::Advanced)
}

/// Whether every frozen task commit in the manifest is an ancestor of `tip` — i.e.
/// the integration already landed on the issue branch.
async fn all_frozen_ancestors(
    repo_path: &Path,
    tip: &str,
    manifest: &FanInManifest,
) -> Result<bool, LoopError> {
    for e in &manifest.ordered {
        if !worktree::is_ancestor(repo_path, &e.sha, tip).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Build the manifest from the current Done-task set. `issue_base_oid` is the
/// issue branch tip (CAS `expected_old`); `ordered` is the Done tasks by
/// `(sort, id)` with their frozen commits.
async fn build_manifest(
    db: &AppDatabase,
    dag: &LoopDagView,
    issue_worktree_folder_id: i32,
) -> Result<FanInManifest, LoopError> {
    let folder = folder_service::get_folder_by_id(&db.conn, issue_worktree_folder_id)
        .await?
        .ok_or_else(|| LoopError::NotFound(format!("worktree folder {issue_worktree_folder_id}")))?;
    let issue_base_oid = worktree::head_commit(Path::new(&folder.path)).await?;

    let mut tasks: Vec<&LoopArtifactRow> = dag
        .artifacts
        .iter()
        .filter(|a| a.kind == ArtifactKind::Task && a.status == ArtifactStatus::Done)
        .collect();
    // `(sort, id)` IS a valid topological order: ingest assigns `sort` by batch
    // index and rejects forward / multi `depends_on` references (backward-only), so
    // a predecessor always has a smaller `sort` than its successor. (Order is in any
    // case non-critical for the final tree — a successor's frozen commit already
    // contains its predecessor's, so out-of-order merges resolve by ancestry — but
    // a topological order keeps the merge sequence and any conflict blame sane.)
    tasks.sort_by(|a, b| a.sort.cmp(&b.sort).then(a.id.cmp(&b.id)));

    // D12: partition Done tasks by contribution. Delta tasks (carry a frozen
    // commit) form the merge order; no-op tasks (declared satisfied, NULL commit)
    // are recorded for provenance and skipped by the merge.
    let mut ordered = Vec::with_capacity(tasks.len());
    let mut skipped_no_op_task_ids = Vec::new();
    for t in &tasks {
        // `fan_in_commit` / `contribution_kind` live on the raw row, not the DTO.
        let row = loop_artifact::Entity::find_by_id(t.id)
            .one(&db.conn)
            .await?
            .ok_or_else(|| LoopError::NotFound(format!("task {}", t.id)))?;
        match row.contribution_kind {
            loop_artifact::ContributionKind::Delta => {
                let sha = row.fan_in_commit.ok_or_else(|| {
                    LoopError::Git(format!(
                        "delta task {} has no frozen commit (invariant)",
                        t.id
                    ))
                })?;
                ordered.push(FanInEntry { task_id: t.id, sha });
            }
            loop_artifact::ContributionKind::NoOp => skipped_no_op_task_ids.push(t.id),
        }
    }

    // Stable epoch anchor (r4 I4): the active task set at claim time, sorted. Every
    // active task is Done here (the run_finalize gate), so this is exactly the
    // partition's union — assert the partition is total so a future bug that drops a
    // task from both buckets fails loudly rather than silently losing it.
    let mut active_task_ids: Vec<i32> = tasks.iter().map(|t| t.id).collect();
    active_task_ids.sort_unstable();
    let mut covered: Vec<i32> = ordered
        .iter()
        .map(|e| e.task_id)
        .chain(skipped_no_op_task_ids.iter().copied())
        .collect();
    covered.sort_unstable();
    if covered != active_task_ids {
        return Err(LoopError::Git(
            "fan-in manifest partition does not cover the active task set (invariant)".into(),
        ));
    }

    Ok(FanInManifest {
        v: 2,
        issue_base_oid,
        active_task_ids,
        ordered,
        skipped_no_op_task_ids,
    })
}

/// Engine-synthesized result capstone (parallel mode produces no agent-submitted
/// result). Idempotent and crash-repairing: a prior partial run that created the
/// row but not its revision / links is completed, not skipped. Links `ResultsFrom`
/// to exactly the manifest's integrated tasks (not the live DAG, which could
/// diverge from what was actually integrated).
async fn create_result_artifact(
    conn: &sea_orm::DatabaseConnection,
    issue: &loop_issue::Model,
    task_ids: &[i32],
) -> Result<(), LoopError> {
    let existing = loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.eq(issue.id))
        .filter(loop_artifact::Column::Kind.eq(ArtifactKind::Result))
        .one(conn)
        .await?;
    let art = match existing {
        Some(a) => a,
        None => {
            loop_service::artifact::create_artifact(
                conn,
                issue.space_id,
                issue.id,
                ArtifactKind::Result,
                "Result",
                ArtifactStatus::Done,
                ActorKind::Agent,
                None,
            )
            .await?
        }
    };

    // Repair-safe: ensure a revision exists (a crash could have created the row
    // alone, and the early-return-on-existing would otherwise leave it empty).
    let has_revision = loop_artifact_revision::Entity::find()
        .filter(loop_artifact_revision::Column::ArtifactId.eq(art.id))
        .one(conn)
        .await?
        .is_some();
    if !has_revision {
        let summary = format!(
            "Integrated {} parallel task(s) into the issue branch.",
            task_ids.len()
        );
        loop_service::artifact::add_revision(conn, art.id, &summary, ActorKind::Agent, None).await?;
    }

    // Ensure a `ResultsFrom` link to each integrated task (skip ones already linked
    // by a prior partial run).
    let linked: std::collections::HashSet<i32> = loop_link::Entity::find()
        .filter(loop_link::Column::FromArtifactId.eq(art.id))
        .filter(loop_link::Column::Kind.eq(LinkKind::ResultsFrom))
        .all(conn)
        .await?
        .into_iter()
        .map(|l| l.to_artifact_id)
        .collect();
    for &task_id in task_ids {
        if !linked.contains(&task_id) {
            loop_service::link::create_link(
                conn,
                issue.space_id,
                art.id,
                task_id,
                LinkKind::ResultsFrom,
                None,
            )
            .await?;
        }
    }
    Ok(())
}

/// Record the dispatch tip and dispatch a result-stage agent to resolve the
/// in-progress fan-in merge. It runs in the **integrate** worktree (so its
/// `git commit` completes the merge there); its briefing (parallel finalize) tells
/// it to resolve conflicts and commit. The recorded `fan_in_resolver_tip` lets a
/// later tick tell "resolver ran and failed" from "crashed before dispatch".
#[allow(clippy::too_many_arguments)]
async fn dispatch_resolver_at(
    db: &AppDatabase,
    data_dir: &Path,
    spawner: &dyn LoopAgentSpawner,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    config: &IssueConfig,
    integrate_worktree_folder_id: i32,
    tip: &str,
) -> Result<StepOutcome, LoopError> {
    set_fan_in_resolver_tip(&db.conn, issue.id, tip).await?;
    let dispatched = dispatch_conflict_resolver(
        db,
        data_dir,
        spawner,
        emitter,
        issue,
        config,
        integrate_worktree_folder_id,
    )
    .await?;
    Ok(if dispatched {
        StepOutcome::Dispatched
    } else {
        StepOutcome::Idle
    })
}

/// Dispatch a result-stage agent (finalize stage) into the integrate worktree.
async fn dispatch_conflict_resolver(
    db: &AppDatabase,
    data_dir: &Path,
    spawner: &dyn LoopAgentSpawner,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    config: &IssueConfig,
    integrate_worktree_folder_id: i32,
) -> Result<bool, LoopError> {
    let spec = resolve_agent_spec(config, Stage::Finalize);
    let handle = dispatch_iteration(
        db,
        data_dir,
        spawner,
        emitter.clone(),
        DispatchInput {
            space_id: issue.space_id,
            issue_id: issue.id,
            stage: Stage::Finalize,
            target_artifact_id: None,
            slot_no: None,
            attempt: 0,
            agent_type: spec.agent,
            mode_id: spec.mode_id,
            config_values: spec.config_values,
            worktree_folder_id: integrate_worktree_folder_id,
        },
    )
    .await?;
    Ok(handle.is_some())
}

/// Block the issue on a structural fan-in fault (unresolved conflict / failed
/// re-validation) with a deduped inbox card, and report `Advanced` so the driver
/// re-ticks and stops on the now-blocked issue.
async fn block_fan_in(
    db: &AppDatabase,
    emitter: &EventEmitter,
    issue: &loop_issue::Model,
    subject_prefix: &str,
    reason: &str,
) -> Result<StepOutcome, LoopError> {
    cas_issue_status(&db.conn, issue.id, IssueStatus::Running, IssueStatus::Blocked).await?;
    loop_service::inbox::upsert_inbox(
        &db.conn,
        issue.space_id,
        issue.id,
        None,
        InboxKind::Blocked,
        &format!("{subject_prefix}:{}", issue.id),
        serde_json::json!({ "v": 1, "reason": reason }),
    )
    .await?;
    emit_changed(emitter, issue.space_id, issue.id, issue.id, "blocked");
    Ok(StepOutcome::Advanced)
}

/// Remove the integrate worktree + force-delete its branch (best-effort — the
/// create path reconciles any leftover).
async fn cleanup_integrate(issue: &loop_issue::Model, repo_path: &Path, integrate_path: &Path) {
    let _ = worktree::remove_worktree(repo_path, integrate_path).await;
    let branch = format!("loop/{}/issue-{}-integrate", issue.space_id, issue.seq_no);
    let _ = worktree::delete_branch(repo_path, &branch, true).await;
}

/// Whether the issue has any queued/running iteration.
async fn issue_has_inflight(db: &AppDatabase, issue_id: i32) -> Result<bool, LoopError> {
    Ok(loop_iteration::Entity::find()
        .filter(loop_iteration::Column::IssueId.eq(issue_id))
        .filter(
            loop_iteration::Column::Status
                .is_in([IterationStatus::Queued, IterationStatus::Running]),
        )
        .one(&db.conn)
        .await?
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D12: the provenance set is delta (merged) ∪ no-op (skipped) — every member,
    /// so the result capstone never drops a no-op task from the lineage.
    #[test]
    fn all_member_task_ids_unions_delta_and_no_op() {
        let m = FanInManifest {
            v: 2,
            issue_base_oid: "base".into(),
            active_task_ids: vec![1, 2, 3, 4],
            ordered: vec![
                FanInEntry { task_id: 1, sha: "a".into() },
                FanInEntry { task_id: 3, sha: "c".into() },
            ],
            skipped_no_op_task_ids: vec![2, 4],
        };
        let mut members = m.all_member_task_ids();
        members.sort_unstable();
        assert_eq!(members, vec![1, 2, 3, 4]);
        // The merge order (ordered_pairs) carries ONLY delta commits.
        assert_eq!(
            m.ordered_pairs(),
            vec![(1, "a".to_string()), (3, "c".to_string())]
        );
    }

    /// A v2 manifest round-trips through the stored-JSON encode/parse path.
    #[test]
    fn manifest_v2_round_trips() {
        let m = FanInManifest {
            v: 2,
            issue_base_oid: "deadbeef".into(),
            active_task_ids: vec![5, 7],
            ordered: vec![FanInEntry { task_id: 5, sha: "s5".into() }],
            skipped_no_op_task_ids: vec![7],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back = parse_manifest(&json).unwrap();
        assert_eq!(back.v, 2);
        assert_eq!(back.active_task_ids, vec![5, 7]);
        assert_eq!(back.skipped_no_op_task_ids, vec![7]);
        assert_eq!(back.all_member_task_ids(), vec![5, 7]);
    }

    /// A pre-D12 (v1) manifest with no no-op fields still parses — the new fields
    /// default to empty, so an all-delta manifest behaves exactly as before.
    #[test]
    fn manifest_v1_shape_parses_with_empty_defaults() {
        let v1 = r#"{"v":1,"issue_base_oid":"base","ordered":[{"task_id":1,"sha":"a"}]}"#;
        let m = parse_manifest(v1).unwrap();
        assert!(m.active_task_ids.is_empty());
        assert!(m.skipped_no_op_task_ids.is_empty());
        assert_eq!(m.all_member_task_ids(), vec![1]);
    }

    /// Codex r1 regression: parse_manifest validates the v2 partition on resume.
    /// A manifest whose `ordered ∪ skipped_no_op_task_ids` does not equal
    /// `active_task_ids` (here task 9 is stranded by corruption / hand-edit) is
    /// rejected rather than silently dropping that task from the merge.
    #[test]
    fn manifest_v2_partition_mismatch_rejected_on_parse() {
        let bad = r#"{"v":2,"issue_base_oid":"base","active_task_ids":[5,7,9],"ordered":[{"task_id":5,"sha":"s5"}],"skipped_no_op_task_ids":[7]}"#;
        let err = parse_manifest(bad).unwrap_err();
        assert!(
            matches!(err, LoopError::Git(_)),
            "partition mismatch should be rejected, got {err:?}"
        );
    }

    /// Codex r2: the all-no_op gate (decided before the landed-recovery check) finishes
    /// ONLY when `tip == base`; a moved tip blocks rather than synthesizing a result
    /// against an unreviewed tip, and a manifest with any frozen commit is not all-no_op.
    #[test]
    fn no_op_gate_finishes_only_at_base() {
        let all_no_op = FanInManifest {
            v: 2,
            issue_base_oid: "base".into(),
            active_task_ids: vec![1, 2],
            ordered: vec![],
            skipped_no_op_task_ids: vec![1, 2],
        };
        assert_eq!(no_op_gate(&all_no_op, "base"), NoOpGate::FinishAtBase);
        assert_eq!(no_op_gate(&all_no_op, "moved"), NoOpGate::BlockMovedTip);

        // A manifest with a frozen commit is never the all-no_op path, regardless of tip.
        let has_delta = FanInManifest {
            v: 2,
            issue_base_oid: "base".into(),
            active_task_ids: vec![1, 2],
            ordered: vec![FanInEntry { task_id: 1, sha: "a".into() }],
            skipped_no_op_task_ids: vec![2],
        };
        assert_eq!(no_op_gate(&has_delta, "base"), NoOpGate::NotAllNoOp);
        assert_eq!(no_op_gate(&has_delta, "moved"), NoOpGate::NotAllNoOp);
    }
}
