//! Process-since-boot engine counters (§2.10b). Cheap relaxed atomics,
//! snapshotted for the health endpoint. Not persisted — they describe *this*
//! process. The authoritative "what's happening now" view (running issues,
//! in-flight iterations, live drivers) is DB- and registry-derived in
//! [`crate::loop_engine::health::LoopEngineHealth`]; these add since-boot context.
//!
//! Intentionally a small, cheaply-reachable subset: incremented only where the
//! engine `self`/`Arc` is already in scope, so no metrics handle is threaded
//! through the engine's free functions (settle/claim/breaker paths). The
//! operational signal that matters most lives in the live counts, not here.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct EngineMetrics {
    /// Iterations settled via the turn-complete event path (the common path; the
    /// rare reconcile-backstop settles are not counted here).
    pub settle_events_total: AtomicU64,
    /// Times the completion watcher fell behind the broadcast buffer and ran a
    /// full in-flight reconcile sweep — i.e. a dropped-event recovery.
    pub lag_sweep_total: AtomicU64,
}

impl EngineMetrics {
    /// Bump a counter (relaxed — these are monotonic tallies, not a sync point).
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            settle_events_total: self.settle_events_total.load(Ordering::Relaxed),
            lag_sweep_total: self.lag_sweep_total.load(Ordering::Relaxed),
        }
    }
}

/// Serializable view of [`EngineMetrics`] for the health endpoint (camelCase to
/// match the TS mirror).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub settle_events_total: u64,
    pub lag_sweep_total: u64,
}
