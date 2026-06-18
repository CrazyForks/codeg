use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// What an iteration's agent run does. (Note: `verify` is NOT a stage — it is a
/// deterministic engine step run between implement and review.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[sea_orm(string_value = "triage")]
    Triage,
    #[sea_orm(string_value = "refine")]
    Refine,
    #[sea_orm(string_value = "design")]
    Design,
    #[sea_orm(string_value = "plan")]
    Plan,
    #[sea_orm(string_value = "implement")]
    Implement,
    #[sea_orm(string_value = "review")]
    Review,
    #[sea_orm(string_value = "finalize")]
    Finalize,
    /// Post-merge memory consolidation: distill durable lessons into a reflection
    /// artifact + space memories. Issue-level (`target = None`), runs on a `Done`
    /// issue, best-effort (never rolls back the merge). See §4.4/P4.
    #[sea_orm(string_value = "reflect")]
    Reflect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    #[sea_orm(string_value = "queued")]
    Queued,
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "succeeded")]
    Succeeded,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "interrupted")]
    Interrupted,
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

/// Why an iteration ended (D11). Settlement/checkpoint write it once; `outcome`
/// is then immutable (see `set_iteration_outcome`). A NULL outcome is legal — the
/// run is still in flight, or it is a settled implement run before its checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum IterationOutcome {
    /// A read-stage run that produced its artifact, or a validated implement.
    #[sea_orm(string_value = "succeeded")]
    Succeeded,
    /// An implement run whose checkpoint found no file changes.
    #[sea_orm(string_value = "empty_diff")]
    EmptyDiff,
    /// An implement run whose deterministic validation failed.
    #[sea_orm(string_value = "validation_failed")]
    ValidationFailed,
    /// Written only in Phase C (agent-declared completion); enumerated now so the
    /// CHECK/UI need no Phase-C edit.
    #[sea_orm(string_value = "declared_complete")]
    DeclaredComplete,
    /// A read-stage run that settled without producing its expected artifact.
    #[sea_orm(string_value = "no_artifacts")]
    NoArtifacts,
    /// Cancelled / failed / interrupted before settling a real outcome.
    #[sea_orm(string_value = "abandoned")]
    Abandoned,
}

/// Who launched the iteration. Engine-driven by default; `human` covers extra
/// turns a person injects while observing. (Distinct from `ActorKind`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum LaunchedBy {
    #[sea_orm(string_value = "engine")]
    Engine,
    #[sea_orm(string_value = "human")]
    Human,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "loop_iteration")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub space_id: i32,
    pub issue_id: i32,
    pub stage: Stage,
    /// Node being advanced/reviewed (plain column, no FK — cycle break).
    pub target_artifact_id: Option<i32>,
    /// Review slot `[0, reviewer_count)`; NULL for non-review stages.
    pub slot_no: Option<i32>,
    /// Backing loop conversation (`conversation.id`, plain column). NULL between
    /// lease acquisition and conversation creation.
    pub conversation_id: Option<i32>,
    /// Unique secret injected into codeg-mcp; the host reverse-looks-up this
    /// iteration's context from it (never trusts agent-supplied ids).
    pub capability_token: String,
    pub status: IterationStatus,
    pub launched_by: LaunchedBy,
    pub attempt: i32,
    pub tokens_used: i64,
    /// `true` when settlement could not read the session file's token total and
    /// left it uncharged; a backfill sweep re-reads and clears this. Never
    /// charged as `0` against the budget while pending (§2.7).
    pub tokens_pending: bool,
    /// JSON-encoded briefing manifest (audit).
    pub context_manifest: Option<String>,
    pub created_at: DateTimeUtc,
    pub started_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    /// Why the run ended (D11). Write-once via `set_iteration_outcome`; NULL while
    /// in flight or for a settled implement run awaiting its checkpoint.
    pub outcome: Option<IterationOutcome>,
    /// D12: the implement agent's free-text reason when it declares the task already
    /// satisfied (`loop_task_complete`), else NULL. Truncated at the write; read by
    /// `finish_implement` (route to review) and the review briefing.
    pub agent_completion_reason: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
