//! DB layer for the loop engineering subsystem. CRUD + read models; the
//! compare-and-swap transitions and dispatch leases live in
//! `loop_engine::transitions`.

pub mod artifact;
pub mod coverage;
pub mod criterion_check;
pub mod criterion_ordinals;
pub mod gate_decision;
pub mod inbox;
pub mod issue;
pub mod iteration;
pub mod link;
pub mod memory;
pub mod space;
pub mod validation;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::loop_artifact::{ArtifactKind, ArtifactStatus};
    use crate::db::entities::loop_artifact_revision::ActorKind;
    use crate::db::entities::loop_inbox_item::{InboxKind, InboxStatus};
    use crate::db::entities::loop_issue::{IssuePriority, IssueStatus};
    use crate::db::entities::loop_criterion::CriterionKind;
    use crate::db::entities::loop_link::LinkKind;
    use crate::db::entities::loop_memory::{MemoryKind, TrustTier};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::loops::IssueConfig;

    #[tokio::test]
    async fn create_issue_seeds_root_artifact() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-a").await;
        let space = space::create_space(&db.conn, "Pay", folder_id).await.unwrap();
        let detail = issue::create_issue(
            &db.conn,
            space.id,
            "Fix webhook",
            "the body",
            IssuePriority::High,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();

        assert_eq!(detail.row.seq_no, 1);
        assert_eq!(detail.row.status, IssueStatus::Pending);

        let dag = artifact::list_dag(&db.conn, detail.row.id).await.unwrap();
        assert_eq!(dag.artifacts.len(), 1, "root artifact created");
        assert_eq!(dag.artifacts[0].kind, ArtifactKind::Issue);
        assert_eq!(dag.artifacts[0].status, ArtifactStatus::Done);

        let det = artifact::get_artifact_detail(&db.conn, dag.artifacts[0].id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(det.revisions.len(), 1, "description seeded as revision 1");
        assert_eq!(det.revisions[0].content, "the body");
    }

    #[tokio::test]
    async fn artifacts_links_idempotent_and_dag() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-b").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let issue_id = issue.row.id;
        let root_id = artifact::list_dag(&db.conn, issue_id).await.unwrap().artifacts[0].id;

        let req = artifact::create_artifact(
            &db.conn,
            space.id,
            issue_id,
            ArtifactKind::Requirement,
            "R1",
            ArtifactStatus::Done,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        artifact::add_revision(&db.conn, req.id, "req body", ActorKind::Agent, None)
            .await
            .unwrap();
        let crit = artifact::add_criterion(&db.conn, req.id, CriterionKind::Acceptance, "must do x")
            .await
            .unwrap();
        assert_eq!(crit.label, "AC-1");
        assert_eq!(crit.kind, CriterionKind::Acceptance);

        // `requirement derives_from issue` — repeated, must dedupe.
        let l1 =
            link::create_link(&db.conn, space.id, req.id, root_id, LinkKind::DerivesFrom, None)
                .await
                .unwrap();
        let l2 =
            link::create_link(&db.conn, space.id, req.id, root_id, LinkKind::DerivesFrom, None)
                .await
                .unwrap();
        assert_eq!(l1.id, l2.id, "link is idempotent");

        let dag = artifact::list_dag(&db.conn, issue_id).await.unwrap();
        assert_eq!(dag.artifacts.len(), 2);
        assert_eq!(dag.links.len(), 1);

        let det = artifact::get_artifact_detail(&db.conn, req.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(det.revisions.len(), 1);
        assert_eq!(det.criteria.len(), 1);
        assert_eq!(det.links.len(), 1);
    }

    #[tokio::test]
    async fn coverage_idempotent_and_typed_criteria() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-cov").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let issue_id = issue.row.id;

        // A requirement with one acceptance criterion + one constraint.
        let req = artifact::create_artifact(
            &db.conn,
            space.id,
            issue_id,
            ArtifactKind::Requirement,
            "R1",
            ArtifactStatus::Done,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let ac = artifact::add_criterion(&db.conn, req.id, CriterionKind::Acceptance, "do x")
            .await
            .unwrap();
        artifact::add_criterion(&db.conn, req.id, CriterionKind::Constraint, "no panics")
            .await
            .unwrap();

        // Kinds round-trip through the detail read.
        let det = artifact::get_artifact_detail(&db.conn, req.id).await.unwrap().unwrap();
        assert_eq!(det.criteria.len(), 2);
        assert_eq!(det.criteria[0].kind, CriterionKind::Acceptance);
        assert_eq!(det.criteria[1].kind, CriterionKind::Constraint);

        // A task covers the acceptance criterion; coverage is idempotent.
        let task = artifact::create_artifact(
            &db.conn,
            space.id,
            issue_id,
            ArtifactKind::Task,
            "T1",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let c1 = coverage::create_coverage(&db.conn, space.id, task.id, ac.id)
            .await
            .unwrap();
        let c2 = coverage::create_coverage(&db.conn, space.id, task.id, ac.id)
            .await
            .unwrap();
        assert_eq!(c1.id, c2.id, "coverage is idempotent");

        // Surfaced both by list_for_issue and inside the DAG view.
        let cov = coverage::list_for_issue(&db.conn, issue_id).await.unwrap();
        assert_eq!(cov.len(), 1);
        assert_eq!(cov[0].task_artifact_id, task.id);
        assert_eq!(cov[0].criterion_id, ac.id);
        let dag = artifact::list_dag(&db.conn, issue_id).await.unwrap();
        assert_eq!(dag.coverage.len(), 1);
        assert_eq!(dag.coverage[0].criterion_id, ac.id);
    }

    #[tokio::test]
    async fn inbox_upsert_tristate_merge_preserve_and_idempotent_handle() {
        use crate::db::service::loop_service::inbox::InboxUpsert;
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-c").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Low,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let iid = issue.row.id;

        // First occurrence → Created, carrying a rich diagnostic payload.
        let first = inbox::upsert_inbox(
            &db.conn,
            space.id,
            iid,
            None,
            InboxKind::Blocked,
            "no_progress:1",
            serde_json::json!({ "failure_sig": "abc", "stage": "implement", "attempt": 1 }),
        )
        .await
        .unwrap();
        assert!(matches!(first, InboxUpsert::Created(_)));
        assert!(first.changed());
        let card_id = first.model().id;

        // Thinner recurrence (only `attempt`) → merge-preserve: failure_sig/stage
        // are kept, attempt updated → Updated, same pending row (Codex r2 N1).
        let second = inbox::upsert_inbox(
            &db.conn,
            space.id,
            iid,
            None,
            InboxKind::Blocked,
            "no_progress:1",
            serde_json::json!({ "attempt": 2 }),
        )
        .await
        .unwrap();
        assert!(matches!(second, InboxUpsert::Updated(_)));
        assert!(second.changed());
        assert_eq!(second.model().id, card_id, "same pending row, not a new card");
        let merged: serde_json::Value = serde_json::from_str(&second.model().payload).unwrap();
        assert_eq!(merged["failure_sig"], "abc", "diagnostic field preserved (N1)");
        assert_eq!(merged["stage"], "implement", "diagnostic field preserved (N1)");
        assert_eq!(merged["attempt"], 2, "new key wins");

        // Identical recurrence → Unchanged, no event (no per-tick spam).
        let third = inbox::upsert_inbox(
            &db.conn,
            space.id,
            iid,
            None,
            InboxKind::Blocked,
            "no_progress:1",
            serde_json::json!({ "attempt": 2 }),
        )
        .await
        .unwrap();
        assert!(matches!(third, InboxUpsert::Unchanged(_)));
        assert!(!third.changed(), "no-op recurrence must not emit");

        // Still exactly one pending card across all three upserts.
        let pending = inbox::list_inbox(&db.conn, space.id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);

        // handle_inbox is idempotent: true on the real transition, false after.
        assert!(inbox::handle_inbox(&db.conn, card_id, serde_json::json!({ "ok": true }))
            .await
            .unwrap());
        assert!(!inbox::handle_inbox(&db.conn, card_id, serde_json::json!({ "ok": true }))
            .await
            .unwrap());
        let still_pending = inbox::list_inbox(&db.conn, space.id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        assert_eq!(still_pending.len(), 0);

        // After the card is handled it no longer occupies the pending slot, so the
        // same key recurs as a fresh Created (no separate "reopened" state).
        let reopened = inbox::upsert_inbox(
            &db.conn,
            space.id,
            iid,
            None,
            InboxKind::Blocked,
            "no_progress:1",
            serde_json::json!({ "attempt": 3 }),
        )
        .await
        .unwrap();
        assert!(matches!(reopened, InboxUpsert::Created(_)));
    }

    #[tokio::test]
    async fn attention_aggregation_buckets_pending_by_class() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-attn").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let i1 = issue::create_issue(
            &db.conn,
            space.id,
            "I1",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let i2 = issue::create_issue(
            &db.conn,
            space.id,
            "I2",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();

        // issue 1: 2 blocking (approval, question) + 1 notice (reflection_failed).
        for (kind, key) in [
            (InboxKind::Approval, "design:1"),
            (InboxKind::Question, "question:1"),
            (InboxKind::ReflectionFailed, "reflect_failed:1"),
        ] {
            inbox::upsert_inbox(&db.conn, space.id, i1.row.id, None, kind, key, serde_json::json!({}))
                .await
                .unwrap();
        }
        // issue 2: 2 blocking (blocked, budget) + 1 approval we then mark handled.
        for (kind, key) in [
            (InboxKind::Blocked, "no_progress:9"),
            (InboxKind::BudgetExhausted, "budget:2"),
            (InboxKind::Approval, "merge:2"),
        ] {
            inbox::upsert_inbox(&db.conn, space.id, i2.row.id, None, kind, key, serde_json::json!({}))
                .await
                .unwrap();
        }
        let merge_id = inbox::list_inbox(&db.conn, space.id, Some(InboxStatus::Pending))
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.subject_key == "merge:2")
            .unwrap()
            .id;
        inbox::handle_inbox(&db.conn, merge_id, serde_json::json!({"ok": true}))
            .await
            .unwrap();

        // Per-space: blocking = approval+question+blocked+budget = 4; notice = 1;
        // the handled approval is excluded.
        let (blocking, notice) = inbox::aggregate_for_space(&db.conn, space.id).await.unwrap();
        assert_eq!((blocking, notice), (4, 1));

        // Per-issue buckets.
        let per_issue = inbox::aggregate_for_issues(&db.conn, &[i1.row.id, i2.row.id])
            .await
            .unwrap();
        assert_eq!(per_issue.get(&i1.row.id).copied(), Some((2, 1)));
        assert_eq!(per_issue.get(&i2.row.id).copied(), Some((2, 0)));

        // Cross-space rollup.
        let all = inbox::aggregate_all(&db.conn).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].space_id, space.id);
        assert_eq!((all[0].blocking, all[0].notice), (4, 1));

        // The space summary carries the same counts.
        let summaries = space::list_spaces(&db.conn).await.unwrap();
        assert_eq!(summaries[0].blocking_count, 4);
        assert_eq!(summaries[0].notice_count, 1);

        // The issue list carries per-issue counts.
        let issues = issue::list_issues(&db.conn, space.id, None).await.unwrap();
        let r1 = issues.iter().find(|r| r.id == i1.row.id).unwrap();
        assert_eq!((r1.blocking_count, r1.notice_count), (2, 1));
    }

    #[tokio::test]
    async fn inbox_subject_resolution_by_family() {
        use crate::db::entities::loop_iteration::{self, IterationStatus, LaunchedBy, Stage};
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-subj").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let iid = issue.row.id;

        let design = artifact::create_artifact(
            &db.conn,
            space.id,
            iid,
            ArtifactKind::Design,
            "Design A",
            ArtifactStatus::AwaitingApproval,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let result = artifact::create_artifact(
            &db.conn,
            space.id,
            iid,
            ArtifactKind::Result,
            "Result A",
            ArtifactStatus::Done,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        let task = artifact::create_artifact(
            &db.conn,
            space.id,
            iid,
            ArtifactKind::Task,
            "Task A",
            ArtifactStatus::Pending,
            ActorKind::Agent,
            None,
        )
        .await
        .unwrap();
        assert_ne!(design.id, iid, "design artifact id differs from issue id (I4 guard)");

        let iter = loop_iteration::ActiveModel {
            space_id: Set(space.id),
            issue_id: Set(iid),
            stage: Set(Stage::Implement),
            target_artifact_id: Set(Some(task.id)),
            capability_token: Set("tok-subj".to_string()),
            status: Set(IterationStatus::Running),
            launched_by: Set(LaunchedBy::Engine),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();

        // One card per subject family.
        let cards = [
            // task-level: payload.task_artifact_id wins over the (deliberately wrong) suffix.
            (
                InboxKind::Blocked,
                "no_progress:99999".to_string(),
                None,
                serde_json::json!({ "task_artifact_id": task.id }),
            ),
            // design-level: suffix is the ISSUE id; resolves to the design artifact.
            (
                InboxKind::Approval,
                format!("design:{iid}"),
                None,
                serde_json::json!({ "gate": "design" }),
            ),
            // result-level: suffix is the ISSUE id; resolves to the result artifact.
            (
                InboxKind::Approval,
                format!("merge:{iid}"),
                None,
                serde_json::json!({ "gate": "merge" }),
            ),
            // iteration-level: resolves to the iteration's target (the task).
            (
                InboxKind::Blocked,
                format!("dispatch_failed:{}", iter.id),
                Some(iter.id),
                serde_json::json!({}),
            ),
            // issue-level: no backing artifact.
            (
                InboxKind::BudgetExhausted,
                format!("budget:{iid}"),
                None,
                serde_json::json!({}),
            ),
            // unknown prefix: no backing artifact.
            (InboxKind::Blocked, "mystery:7".to_string(), None, serde_json::json!({})),
        ];
        for (kind, key, it, payload) in cards {
            inbox::upsert_inbox(&db.conn, space.id, iid, it, kind, &key, payload)
                .await
                .unwrap();
        }

        let rows = inbox::list_inbox(&db.conn, space.id, Some(InboxStatus::Pending))
            .await
            .unwrap();
        let by_key = |k: &str| rows.iter().find(|r| r.subject_key == k).unwrap();

        let np = by_key("no_progress:99999");
        assert_eq!(np.subject_artifact_id, Some(task.id), "payload id wins over suffix");
        assert_eq!(np.subject_title.as_deref(), Some("Task A"));

        let d = by_key(&format!("design:{iid}"));
        assert_eq!(d.subject_artifact_id, Some(design.id), "design id, NOT issue id (I4)");
        assert_eq!(d.subject_title.as_deref(), Some("Design A"));

        let m = by_key(&format!("merge:{iid}"));
        assert_eq!(m.subject_artifact_id, Some(result.id));
        assert_eq!(m.subject_title.as_deref(), Some("Result A"));

        let df = by_key(&format!("dispatch_failed:{}", iter.id));
        assert_eq!(df.subject_artifact_id, Some(task.id), "iteration target");

        assert_eq!(by_key(&format!("budget:{iid}")).subject_artifact_id, None);
        assert_eq!(by_key("mystery:7").subject_artifact_id, None);
    }

    #[tokio::test]
    async fn set_iteration_outcome_is_write_once() {
        use crate::db::entities::loop_iteration::{
            self, IterationOutcome, IterationStatus, LaunchedBy, Stage,
        };
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-wo").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();
        let iter = loop_iteration::ActiveModel {
            space_id: Set(space.id),
            issue_id: Set(issue.row.id),
            stage: Set(Stage::Refine),
            capability_token: Set("tok-wo".to_string()),
            status: Set(IterationStatus::Running),
            launched_by: Set(LaunchedBy::Engine),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();

        // First write succeeds (outcome was NULL).
        assert!(iteration::set_iteration_outcome(&db.conn, iter.id, IterationOutcome::Succeeded)
            .await
            .unwrap());
        // A later/stale write is a no-op and must NOT clobber the real outcome (C2).
        assert!(!iteration::set_iteration_outcome(&db.conn, iter.id, IterationOutcome::Abandoned)
            .await
            .unwrap());
        let after = loop_iteration::Entity::find_by_id(iter.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.outcome,
            Some(IterationOutcome::Succeeded),
            "write-once preserves the real outcome"
        );
    }

    #[tokio::test]
    async fn space_summary_and_cascade_delete() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-d").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();
        let issue = issue::create_issue(
            &db.conn,
            space.id,
            "I",
            "d",
            IssuePriority::Medium,
            Some(&IssueConfig::default()),
        )
        .await
        .unwrap();

        let summaries = space::list_spaces(&db.conn).await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].issue_count, 1);
        assert!(!summaries[0].detached, "live folder is not detached");

        space::delete_space(&db.conn, space.id).await.unwrap();
        // FK cascade removed the issue and its root artifact.
        let dag = artifact::list_dag(&db.conn, issue.row.id).await.unwrap();
        assert_eq!(dag.artifacts.len(), 0, "cascade removed artifacts");
        assert!(space::list_spaces(&db.conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_crud() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/repo-e").await;
        let space = space::create_space(&db.conn, "S", folder_id).await.unwrap();

        memory::create_memory(
            &db.conn,
            space.id,
            MemoryKind::Pitfall,
            ActorKind::Agent,
            "p",
            None,
            "b",
            TrustTier::Proposed,
            memory::MemoryProvenance::default(),
        )
        .await
        .unwrap();
        memory::create_memory(
            &db.conn,
            space.id,
            MemoryKind::Decision,
            ActorKind::Human,
            "d",
            None,
            "b",
            TrustTier::Human,
            memory::MemoryProvenance::default(),
        )
        .await
        .unwrap();
        assert_eq!(memory::list_memory(&db.conn, space.id).await.unwrap().len(), 2);
    }
}
