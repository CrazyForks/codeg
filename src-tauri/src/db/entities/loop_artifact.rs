use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use super::loop_artifact_revision::ActorKind;

/// DAG node kind = column in the per-issue lineage graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[sea_orm(string_value = "issue")]
    Issue,
    #[sea_orm(string_value = "requirement")]
    Requirement,
    #[sea_orm(string_value = "design")]
    Design,
    #[sea_orm(string_value = "task")]
    Task,
    #[sea_orm(string_value = "review")]
    Review,
    #[sea_orm(string_value = "result")]
    Result,
    /// Post-merge retrospective produced by the reflect stage; `derives_from` the
    /// issue's result (else its root). At most one per issue (the durable memory
    /// consolidation idempotency anchor, `uniq_reflection_per_issue`). See §4.4/P4.
    #[sea_orm(string_value = "reflection")]
    Reflection,
}

/// Engine-driven node status (humans never hand-edit these except via gates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "in_progress")]
    InProgress,
    #[sea_orm(string_value = "awaiting_approval")]
    AwaitingApproval,
    #[sea_orm(string_value = "done")]
    Done,
    #[sea_orm(string_value = "blocked")]
    Blocked,
    #[sea_orm(string_value = "superseded")]
    Superseded,
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

/// Verdict carried only by `kind = review` artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    #[sea_orm(string_value = "pass")]
    Pass,
    #[sea_orm(string_value = "fail")]
    Fail,
}

/// Whether a Done task contributed a real diff (its frozen `fan_in_commit`) or was
/// an agent-declared no-op (already satisfied; `fan_in_commit IS NULL`). Only
/// meaningful for parallel fan-in participants (D12); serial tasks always record
/// `Delta` (the column is not read for serial issues).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ContributionKind {
    #[sea_orm(string_value = "delta")]
    Delta,
    #[sea_orm(string_value = "no_op")]
    NoOp,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_artifact")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub space_id: i32,
    pub issue_id: i32,
    pub kind: ArtifactKind,
    pub title: String,
    pub status: ArtifactStatus,
    pub origin: ActorKind,
    /// Iteration that produced this node (plain column, no FK — cycle break).
    pub produced_by_iteration_id: Option<i32>,
    /// Only set for `kind = review`.
    pub verdict: Option<ReviewVerdict>,
    /// Node-level rework counter (no-progress circuit breaker reads this).
    pub attempt: i32,
    pub last_failure_sig: Option<String>,
    /// Frozen integration commit SHA, recorded atomically when a `task` turns
    /// `Done` (its accepted tip). The parallel result-stage fan-in merges these
    /// SHAs, never live branch tips. `NULL` for non-task kinds / not-yet-done.
    pub fan_in_commit: Option<String>,
    pub sort: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// D12: real-diff (`Delta`) vs agent-declared no-op (`NoOp`) for a Done task.
    /// Defaults to `Delta`; `no_op ⇔ fan_in_commit IS NULL` (parallel fan-in).
    pub contribution_kind: ContributionKind,
    /// D14: oscillation breaker epoch counter (consecutive same-signature blocks).
    pub oscillation_count: i32,
    /// D14: the `block_sig` of the current oscillation epoch (NULL when not blocked
    /// in an epoch). Stepped/reset together with `oscillation_count`.
    pub recent_failure_sig: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
