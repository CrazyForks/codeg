//! Engine health snapshot for the workbench badge + ops (§2.10b).

use crate::loop_engine::metrics::MetricsSnapshot;

/// Live engine health. The counts are authoritative *now*: issues/iterations
/// from the DB, drivers from the in-process registry. `metrics` is this
/// process's since-boot tally. An operator reads this to spot trouble at a
/// glance — drivers lagging the running-issue count, token settlements piling up
/// `pending`, or repeated lag-sweep recoveries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoopEngineHealth {
    /// Issues currently in `running` (DB).
    pub running_issues: u64,
    /// Iterations currently `queued` or `running` (DB).
    pub in_flight_iterations: u64,
    /// Settled iterations whose token total is still `pending` a backfill (DB).
    pub pending_token_iterations: u64,
    /// Live per-issue driver tasks in the registry (in-process).
    pub active_drivers: u64,
    /// Process-since-boot counters.
    pub metrics: MetricsSnapshot,
}
