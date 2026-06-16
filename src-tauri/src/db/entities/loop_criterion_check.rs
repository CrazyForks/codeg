use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One reviewer's structured pass/fail of one criterion (§3.4) — the unit the
/// gate aggregates. Scoped to the artifact judged: a task (per-task review) or
/// the result (integration review). Idempotent on `(criterion, iteration, scope)`
/// so a crash replay of a review submission never double-writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum CheckVerdict {
    #[sea_orm(string_value = "pass")]
    Pass,
    #[sea_orm(string_value = "fail")]
    Fail,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_criterion_check")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub space_id: i32,
    pub criterion_id: i32,
    /// The review iteration that produced this check (its slot identifies the
    /// reviewer for per-criterion quorum aggregation).
    pub iteration_id: i32,
    /// The artifact this check judged: a task, or the result for the integration
    /// gate.
    pub scope_artifact_id: i32,
    pub verdict: CheckVerdict,
    pub evidence: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
