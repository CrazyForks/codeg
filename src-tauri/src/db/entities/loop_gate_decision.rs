use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The aggregated outcome of a gate over one target at one attempt (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    #[sea_orm(string_value = "pass")]
    Pass,
    #[sea_orm(string_value = "fail")]
    Fail,
    #[sea_orm(string_value = "undecided")]
    Undecided,
}

/// Immutable gate-decision audit: which structured checks a gate aggregated and
/// the outcome it reached, at `(target, stage, attempt)`. `input_check_ids` is the
/// JSON id list it aggregated; `input_digest` fingerprints those inputs so a
/// racing recompute is detected (insert-or-compare) and a later check supersede
/// never rewrites a recorded decision.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_gate_decision")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub space_id: i32,
    pub issue_id: i32,
    pub target_artifact_id: i32,
    /// The gate's stage label (e.g. `review` for a task gate, `finalize` for the
    /// integration gate).
    pub stage: String,
    pub attempt: i32,
    pub policy_json: String,
    pub input_check_ids: String,
    pub input_digest: String,
    pub outcome: GateOutcome,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
