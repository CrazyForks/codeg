use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Criterion category — the typed unit of traceability and gating. Requirements
/// and tasks carry only `acceptance` (verifiable outcomes a task must satisfy);
/// designs carry `constraint`/`invariant`/`obligation` (cross-cutting properties
/// the implementation must uphold, never dropped on the floor). ingest enforces
/// the per-artifact-kind allow-set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum CriterionKind {
    #[sea_orm(string_value = "acceptance")]
    Acceptance,
    #[sea_orm(string_value = "constraint")]
    Constraint,
    #[sea_orm(string_value = "invariant")]
    Invariant,
    #[sea_orm(string_value = "obligation")]
    Obligation,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_criterion")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Owning artifact (design or task). Reviews judge these criteria.
    pub artifact_id: i32,
    /// Auto-assigned label like `AC-1`.
    pub label: String,
    pub text: String,
    pub sort: i32,
    pub kind: CriterionKind,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
