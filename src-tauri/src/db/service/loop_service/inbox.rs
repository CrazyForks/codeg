use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

use crate::db::entities::loop_artifact::{self, ArtifactKind, ArtifactStatus};
use crate::db::entities::loop_inbox_item::{self, InboxKind, InboxStatus};
use crate::db::entities::{loop_issue, loop_iteration};
use crate::db::error::DbError;
use crate::models::loops::{LoopInboxItemRow, LoopSpaceAttention};

fn to_row(m: loop_inbox_item::Model, issue_seq: i32) -> LoopInboxItemRow {
    LoopInboxItemRow {
        id: m.id,
        issue_id: m.issue_id,
        issue_seq,
        iteration_id: m.iteration_id,
        kind: m.kind,
        subject_key: m.subject_key,
        payload: serde_json::from_str(&m.payload).unwrap_or(serde_json::Value::Null),
        status: m.status,
        // Resolved at read time in `list_inbox` (B4); default for non-list callers.
        subject_artifact_id: None,
        subject_title: None,
        created_at: m.created_at,
    }
}

/// Whether a pending inbox card demands a human before the issue can proceed
/// (`Blocking`) or is merely informational (`Notice`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttentionClass {
    Blocking,
    Notice,
}

/// Classify an inbox kind for the attention rollup (D6). Exhaustive over the typed
/// `InboxKind` — no `_` arm, so a future kind forces a compile error here until it
/// is classified (never silently dropped). `question` is Blocking: a pending agent
/// question needs the human to answer before the issue can advance (resolves the
/// spec D6 gap, which omitted `question`).
pub fn attention_class(kind: InboxKind) -> AttentionClass {
    match kind {
        InboxKind::Approval
        | InboxKind::Blocked
        | InboxKind::BudgetExhausted
        | InboxKind::Question => AttentionClass::Blocking,
        InboxKind::ReflectionFailed => AttentionClass::Notice,
    }
}

/// Fold pending-card kinds into `(blocking, notice)` counts.
fn tally(kinds: impl IntoIterator<Item = InboxKind>) -> (i64, i64) {
    let (mut blocking, mut notice) = (0i64, 0i64);
    for k in kinds {
        match attention_class(k) {
            AttentionClass::Blocking => blocking += 1,
            AttentionClass::Notice => notice += 1,
        }
    }
    (blocking, notice)
}

/// `(blocking, notice)` pending-inbox counts for one space (D6). Selects only the
/// `kind` column (never the payload) and classifies in Rust — the pending set is
/// small and this avoids a fragile SQL `GROUP BY` over enum strings.
pub async fn aggregate_for_space(
    conn: &impl sea_orm::ConnectionTrait,
    space_id: i32,
) -> Result<(i64, i64), DbError> {
    let kinds: Vec<InboxKind> = loop_inbox_item::Entity::find()
        .select_only()
        .column(loop_inbox_item::Column::Kind)
        .filter(loop_inbox_item::Column::SpaceId.eq(space_id))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .into_tuple::<InboxKind>()
        .all(conn)
        .await?;
    Ok(tally(kinds))
}

/// `(blocking, notice)` pending-inbox counts per issue (D6). Issues with no
/// pending cards are absent from the map (the caller defaults them to 0). One
/// batched query for the whole issue list — no N+1.
pub async fn aggregate_for_issues(
    conn: &impl sea_orm::ConnectionTrait,
    issue_ids: &[i32],
) -> Result<HashMap<i32, (i64, i64)>, DbError> {
    if issue_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i32, InboxKind)> = loop_inbox_item::Entity::find()
        .select_only()
        .column(loop_inbox_item::Column::IssueId)
        .column(loop_inbox_item::Column::Kind)
        .filter(loop_inbox_item::Column::IssueId.is_in(issue_ids.to_vec()))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .into_tuple::<(i32, InboxKind)>()
        .all(conn)
        .await?;
    let mut map: HashMap<i32, (i64, i64)> = HashMap::new();
    for (issue_id, kind) in rows {
        let entry = map.entry(issue_id).or_insert((0, 0));
        match attention_class(kind) {
            AttentionClass::Blocking => entry.0 += 1,
            AttentionClass::Notice => entry.1 += 1,
        }
    }
    Ok(map)
}

/// Per-space attention across ALL spaces — the global "who needs me" rollup (D6/D7).
/// Sorted by `space_id` for stable output.
pub async fn aggregate_all(
    conn: &impl sea_orm::ConnectionTrait,
) -> Result<Vec<LoopSpaceAttention>, DbError> {
    let rows: Vec<(i32, InboxKind)> = loop_inbox_item::Entity::find()
        .select_only()
        .column(loop_inbox_item::Column::SpaceId)
        .column(loop_inbox_item::Column::Kind)
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .into_tuple::<(i32, InboxKind)>()
        .all(conn)
        .await?;
    let mut map: HashMap<i32, (i64, i64)> = HashMap::new();
    for (space_id, kind) in rows {
        let entry = map.entry(space_id).or_insert((0, 0));
        match attention_class(kind) {
            AttentionClass::Blocking => entry.0 += 1,
            AttentionClass::Notice => entry.1 += 1,
        }
    }
    let mut per_space: Vec<LoopSpaceAttention> = map
        .into_iter()
        .map(|(space_id, (blocking, notice))| LoopSpaceAttention {
            space_id,
            blocking,
            notice,
        })
        .collect();
    per_space.sort_by_key(|s| s.space_id);
    Ok(per_space)
}

/// Outcome of [`upsert_inbox`]. Lets callers emit `loop://changed` only on a real
/// change (`Created`/`Updated`) and stay silent on a no-op recurrence
/// (`Unchanged`), so a card repeated every driver tick never spams the realtime
/// channel. The resulting row is carried in every variant.
pub enum InboxUpsert {
    Created(loop_inbox_item::Model),
    Updated(loop_inbox_item::Model),
    Unchanged(loop_inbox_item::Model),
}

impl InboxUpsert {
    /// The resulting row, whether or not it changed.
    pub fn into_model(self) -> loop_inbox_item::Model {
        match self {
            InboxUpsert::Created(m) | InboxUpsert::Updated(m) | InboxUpsert::Unchanged(m) => m,
        }
    }

    /// Borrow the resulting row.
    pub fn model(&self) -> &loop_inbox_item::Model {
        match self {
            InboxUpsert::Created(m) | InboxUpsert::Updated(m) | InboxUpsert::Unchanged(m) => m,
        }
    }

    /// True when a card was created or its payload changed — i.e. when the caller
    /// should emit a realtime change event. `Unchanged` returns false.
    pub fn changed(&self) -> bool {
        matches!(self, InboxUpsert::Created(_) | InboxUpsert::Updated(_))
    }
}

/// Shallow-merge `new` over `base`: when both are JSON objects, each key of `new`
/// overwrites/extends `base` (new keys win, base-only keys preserved). Otherwise
/// `new` replaces `base` wholesale. Preserves diagnostic fields (`failure_sig`,
/// `attempt`, `stage`, output tails) that a thinner recurrence omits (Codex r2 N1).
fn shallow_merge(base: serde_json::Value, new: serde_json::Value) -> serde_json::Value {
    match (base, new) {
        (serde_json::Value::Object(mut b), serde_json::Value::Object(n)) => {
            for (k, v) in n {
                b.insert(k, v);
            }
            serde_json::Value::Object(b)
        }
        (_, new) => new,
    }
}

/// Insert a pending inbox card, or fold a recurrence into the existing pending one
/// with the same `(issue_id, kind, subject_key)` — recovery and repeated ticks
/// must not stack duplicate cards (also guarded by `uniq_inbox_pending`).
///
/// On recurrence the new payload is **merge-preserved** over the existing one
/// (never dropping fields a thinner payload omits): an equal merge yields
/// `Unchanged` (no write, no event); a differing merge is persisted and yields
/// `Updated`. A first occurrence yields `Created`.
pub async fn upsert_inbox(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    issue_id: i32,
    iteration_id: Option<i32>,
    kind: InboxKind,
    subject_key: &str,
    payload: serde_json::Value,
) -> Result<InboxUpsert, DbError> {
    if let Some(existing) = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::IssueId.eq(issue_id))
        .filter(loop_inbox_item::Column::Kind.eq(kind))
        .filter(loop_inbox_item::Column::SubjectKey.eq(subject_key))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .one(conn)
        .await?
    {
        let existing_payload: serde_json::Value =
            serde_json::from_str(&existing.payload).unwrap_or(serde_json::Value::Null);
        let merged = shallow_merge(existing_payload.clone(), payload);
        if merged == existing_payload {
            return Ok(InboxUpsert::Unchanged(existing));
        }
        let mut active = existing.into_active_model();
        active.payload = Set(merged.to_string());
        return Ok(InboxUpsert::Updated(active.update(conn).await?));
    }
    let inserted = loop_inbox_item::ActiveModel {
        space_id: Set(space_id),
        issue_id: Set(issue_id),
        iteration_id: Set(iteration_id),
        kind: Set(kind),
        subject_key: Set(subject_key.to_string()),
        payload: Set(payload.to_string()),
        status: Set(InboxStatus::Pending),
        resolution: Set(None),
        created_at: Set(Utc::now()),
        handled_at: Set(None),
        ..Default::default()
    }
    .insert(conn)
    .await?;
    Ok(InboxUpsert::Created(inserted))
}

/// How a card's `subject_key` resolves to the artifact it concerns (D9). The
/// `{prefix}:{id}` suffix means different things per family, so the prefix is
/// classified first and the id resolved accordingly — an issue-keyed suffix is
/// NEVER treated as an artifact id (Codex r1 I4).
enum SubjectResolution {
    /// Task-level: the id IS a task artifact id (task ≡ artifact).
    Artifact(i32),
    /// The named issue's live design artifact.
    DesignOf(i32),
    /// The named issue's live result artifact.
    ResultOf(i32),
    /// The named iteration's `target_artifact_id`.
    IterationTarget(i32),
    /// Issue-level card (no backing artifact) or an unknown prefix.
    None,
}

/// Split a `{prefix}:{id}` subject key. Returns `None` if it has no integer tail.
fn parse_subject_key(key: &str) -> Option<(&str, i32)> {
    let (prefix, rest) = key.split_once(':')?;
    Some((prefix, rest.parse::<i32>().ok()?))
}

/// Classify a card into its resolution intent (no DB access). `iteration_id` is
/// the card's column (authoritative for iteration-keyed cards).
fn classify_subject(
    subject_key: &str,
    payload: &serde_json::Value,
    iteration_id: Option<i32>,
) -> SubjectResolution {
    let parsed = parse_subject_key(subject_key);
    let prefix = parsed.map(|(p, _)| p).unwrap_or("");
    let suffix = parsed.map(|(_, id)| id);
    match prefix {
        // task ≡ artifact: prefer the payload's explicit artifact id; the suffix is
        // itself the task artifact id (no separate task id), so it is a safe fallback.
        "no_progress" | "validation_blocked" | "infra_failure" | "oscillation" => payload
            .get("task_artifact_id")
            .or_else(|| payload.get("node_artifact_id"))
            .and_then(|v| v.as_i64())
            .map(|n| n as i32)
            .or(suffix)
            .map(SubjectResolution::Artifact)
            .unwrap_or(SubjectResolution::None),
        // issue-keyed: the suffix is an ISSUE id — resolve via the issue's artifact,
        // never as an artifact id directly.
        "design" | "design_rejected" => {
            suffix.map(SubjectResolution::DesignOf).unwrap_or(SubjectResolution::None)
        }
        "merge" | "merge_blocked" | "merge_rejected" | "finalize_dirty" | "unverifiable"
        | "integration_gap" => {
            suffix.map(SubjectResolution::ResultOf).unwrap_or(SubjectResolution::None)
        }
        // iteration-keyed: the `iteration_id` column is authoritative. `dispatch_failed`
        // / `stalled` also carry it as the suffix; `question`'s suffix is a question id
        // (NOT an iteration id), so it relies on the column only.
        "dispatch_failed" | "stalled" => iteration_id
            .or(suffix)
            .map(SubjectResolution::IterationTarget)
            .unwrap_or(SubjectResolution::None),
        "question" => iteration_id
            .map(SubjectResolution::IterationTarget)
            .unwrap_or(SubjectResolution::None),
        // issue-level cards (budget, coverage_gap, …) and any unknown prefix have no
        // backing artifact → the frontend roots them at the issue.
        _ => SubjectResolution::None,
    }
}

/// `issue_id → live artifact id` for one kind (latest non-dead). Batched (no N+1).
async fn live_artifact_by_issue(
    conn: &sea_orm::DatabaseConnection,
    issue_ids: &[i32],
    kind: ArtifactKind,
) -> Result<HashMap<i32, i32>, DbError> {
    if issue_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut map = HashMap::new();
    // Ascending id, overwrite → the highest live id (the latest) wins per issue.
    for a in loop_artifact::Entity::find()
        .filter(loop_artifact::Column::IssueId.is_in(issue_ids.to_vec()))
        .filter(loop_artifact::Column::Kind.eq(kind))
        .filter(loop_artifact::Column::Status.ne(ArtifactStatus::Superseded))
        .filter(loop_artifact::Column::Status.ne(ArtifactStatus::Cancelled))
        .order_by_asc(loop_artifact::Column::Id)
        .all(conn)
        .await?
    {
        map.insert(a.issue_id, a.id);
    }
    Ok(map)
}

pub async fn list_inbox(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    status: Option<InboxStatus>,
) -> Result<Vec<LoopInboxItemRow>, DbError> {
    let seqs: HashMap<i32, i32> = loop_issue::Entity::find()
        .filter(loop_issue::Column::SpaceId.eq(space_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|i| (i.id, i.seq_no))
        .collect();
    let mut query = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::SpaceId.eq(space_id))
        .order_by_desc(loop_inbox_item::Column::Id);
    if let Some(status) = status {
        query = query.filter(loop_inbox_item::Column::Status.eq(status));
    }
    let models = query.all(conn).await?;

    // Classify every card's subject (no DB access), then resolve in a few batched
    // queries (D9): issue→design, issue→result, iteration→target, then id→title.
    let resolutions: Vec<SubjectResolution> = models
        .iter()
        .map(|m| {
            let payload = serde_json::from_str(&m.payload).unwrap_or(serde_json::Value::Null);
            classify_subject(&m.subject_key, &payload, m.iteration_id)
        })
        .collect();

    let (mut design_issue_ids, mut result_issue_ids, mut iteration_ids) =
        (Vec::new(), Vec::new(), Vec::new());
    for r in &resolutions {
        match r {
            SubjectResolution::DesignOf(id) => design_issue_ids.push(*id),
            SubjectResolution::ResultOf(id) => result_issue_ids.push(*id),
            SubjectResolution::IterationTarget(id) => iteration_ids.push(*id),
            SubjectResolution::Artifact(_) | SubjectResolution::None => {}
        }
    }

    let design_by_issue = live_artifact_by_issue(conn, &design_issue_ids, ArtifactKind::Design).await?;
    let result_by_issue = live_artifact_by_issue(conn, &result_issue_ids, ArtifactKind::Result).await?;
    let target_by_iter: HashMap<i32, Option<i32>> = if iteration_ids.is_empty() {
        HashMap::new()
    } else {
        loop_iteration::Entity::find()
            .filter(loop_iteration::Column::Id.is_in(iteration_ids))
            .all(conn)
            .await?
            .into_iter()
            .map(|it| (it.id, it.target_artifact_id))
            .collect()
    };

    // Resolve each card to its artifact id (Option), then fetch all titles at once.
    let resolved_ids: Vec<Option<i32>> = resolutions
        .iter()
        .map(|r| match r {
            SubjectResolution::Artifact(id) => Some(*id),
            SubjectResolution::DesignOf(id) => design_by_issue.get(id).copied(),
            SubjectResolution::ResultOf(id) => result_by_issue.get(id).copied(),
            SubjectResolution::IterationTarget(id) => target_by_iter.get(id).copied().flatten(),
            SubjectResolution::None => None,
        })
        .collect();

    let artifact_ids: Vec<i32> = resolved_ids.iter().flatten().copied().collect();
    let titles: HashMap<i32, String> = if artifact_ids.is_empty() {
        HashMap::new()
    } else {
        loop_artifact::Entity::find()
            .filter(loop_artifact::Column::Id.is_in(artifact_ids))
            .all(conn)
            .await?
            .into_iter()
            .map(|a| (a.id, a.title))
            .collect()
    };

    Ok(models
        .into_iter()
        .zip(resolved_ids)
        .map(|(m, art_id)| {
            let seq = *seqs.get(&m.issue_id).unwrap_or(&0);
            let mut row = to_row(m, seq);
            row.subject_artifact_id = art_id;
            row.subject_title = art_id.and_then(|id| titles.get(&id).cloned());
            row
        })
        .collect())
}

/// Fetch a single inbox item by id — used by the command layer to guard a
/// dismiss to informational cards before marking it handled.
pub async fn get_inbox(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
) -> Result<Option<loop_inbox_item::Model>, DbError> {
    Ok(loop_inbox_item::Entity::find_by_id(id).one(conn).await?)
}

/// Mark a pending card handled. Returns `true` if it actually transitioned a
/// pending card to handled, `false` if it was already handled (idempotent) — so
/// callers emit `loop://changed` (the badge dropping) only on a real change.
pub async fn handle_inbox(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
    resolution: serde_json::Value,
) -> Result<bool, DbError> {
    let row = loop_inbox_item::Entity::find_by_id(id)
        .one(conn)
        .await?
        .ok_or_else(|| {
            DbError::Database(sea_orm::DbErr::RecordNotFound(format!("loop_inbox_item {id}")))
        })?;
    if row.status == InboxStatus::Handled {
        return Ok(false);
    }
    let mut active = row.into_active_model();
    active.status = Set(InboxStatus::Handled);
    active.resolution = Set(Some(resolution.to_string()));
    active.handled_at = Set(Some(Utc::now()));
    active.update(conn).await?;
    Ok(true)
}
