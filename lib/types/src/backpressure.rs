//! Global backpressure controller.
//!
//! Follows the same singleton pattern as `ComponentStateReporter`:
//! any component calls `BackpressureHandle::global()` without constructor injection.
//!
//! Multiple components can independently signal overload via `set_overloaded()`.
//! The node returns to `Accepting` only when every active condition is cleared.

use crate::backpressure_metrics::BACKPRESSURE_METRICS;
use crate::{NotAcceptingReason, OverloadCause, TransactionAcceptanceState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;

#[derive(Default)]
struct Inner {
    permanent: Option<NotAcceptingReason>,
    active: HashMap<OverloadCause, u64>, // cause -> retry_after_ms
}

#[derive(Clone)]
pub struct BackpressureHandle {
    inner: Arc<Mutex<Inner>>,
    sender: Arc<watch::Sender<TransactionAcceptanceState>>,
}

impl BackpressureHandle {
    /// Returns the process-wide singleton. Initializes on first call.
    /// Components call this directly — no constructor injection required.
    /// Same pattern as `ComponentStateReporter::global()`.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<BackpressureHandle> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let (tx, _) = watch::channel(TransactionAcceptanceState::Accepting);
            Self::new_internal(tx)
        })
    }

    /// Subscribe to state changes. Pass the returned receiver to `TxHandler`.
    pub fn subscribe(&self) -> watch::Receiver<TransactionAcceptanceState> {
        self.sender.subscribe()
    }

    /// Signal that a component is overloaded. Idempotent — calling again updates the retry hint.
    pub fn set_overloaded(&self, cause: OverloadCause, retry_after_ms: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.permanent.is_some() {
            return;
        }
        inner.active.insert(cause, retry_after_ms);
        BACKPRESSURE_METRICS.active[&cause.as_rpc_str()].set(1);
        self.sync(&inner);
    }

    /// Signal that a component has recovered. No-op if the cause was not active.
    pub fn clear_overloaded(&self, cause: OverloadCause) {
        let mut inner = self.inner.lock().unwrap();
        if inner.permanent.is_some() {
            return;
        }
        inner.active.remove(&cause);
        BACKPRESSURE_METRICS.active[&cause.as_rpc_str()].set(0);
        self.sync(&inner);
    }

    /// Permanently stop accepting transactions. Cannot be undone without a node restart.
    /// Used by `BlockExecutor` when `max_blocks_to_produce` limit is hit.
    pub fn stop_permanently(&self, reason: NotAcceptingReason) {
        let mut inner = self.inner.lock().unwrap();
        inner.permanent = Some(reason);
        BACKPRESSURE_METRICS.active[&"block_production_disabled"].set(1);
        self.sender
            .send_replace(TransactionAcceptanceState::NotAccepting(reason));
    }

    /// Read the current state without awaiting a change.
    pub fn current(&self) -> TransactionAcceptanceState {
        self.sender.borrow().clone()
    }

    // ── Internal ────────────────────────────────────────────────────────────

    fn new_internal(sender: watch::Sender<TransactionAcceptanceState>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            sender: Arc::new(sender),
        }
    }

    /// For unit tests only — creates an isolated handle not connected to the global.
    #[cfg(test)]
    pub fn new_for_test(sender: watch::Sender<TransactionAcceptanceState>) -> Self {
        Self::new_internal(sender)
    }

    fn sync(&self, inner: &Inner) {
        let state = if inner.active.is_empty() {
            TransactionAcceptanceState::Accepting
        } else {
            // Pick the condition with the longest suggested retry delay.
            let (&cause, &retry_after_ms) = inner.active.iter().max_by_key(|(_, ms)| *ms).unwrap();
            TransactionAcceptanceState::NotAccepting(NotAcceptingReason::Overloaded {
                cause,
                retry_after_ms,
            })
        };
        // Use send_replace so the stored value is always updated, even when no receivers exist.
        // This ensures `current()` reflects the latest state in tests and during shutdown.
        self.sender.send_replace(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NotAcceptingReason, OverloadCause, TransactionAcceptanceState};

    fn make_handle() -> BackpressureHandle {
        let (tx, _) = tokio::sync::watch::channel(TransactionAcceptanceState::Accepting);
        BackpressureHandle::new_for_test(tx)
    }

    #[test]
    fn starts_accepting() {
        let h = make_handle();
        assert!(matches!(h.current(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn set_overloaded_signals_not_accepting() {
        let h = make_handle();
        h.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        assert!(matches!(
            h.current(),
            TransactionAcceptanceState::NotAccepting(_)
        ));
    }

    #[test]
    fn clear_overloaded_returns_to_accepting() {
        let h = make_handle();
        h.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        h.clear_overloaded(OverloadCause::ProverQueueFull);
        assert!(matches!(h.current(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn two_conditions_both_must_clear() {
        let h = make_handle();
        h.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        h.set_overloaded(OverloadCause::PipelineSaturated, 1_000);
        h.clear_overloaded(OverloadCause::ProverQueueFull);
        // Still overloaded — pipeline not cleared yet
        assert!(matches!(
            h.current(),
            TransactionAcceptanceState::NotAccepting(_)
        ));
        h.clear_overloaded(OverloadCause::PipelineSaturated);
        assert!(matches!(h.current(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn stop_permanently_blocks_all_clearing() {
        let h = make_handle();
        h.stop_permanently(NotAcceptingReason::BlockProductionDisabled);
        h.clear_overloaded(OverloadCause::ProverQueueFull); // should be a no-op
        assert!(matches!(
            h.current(),
            TransactionAcceptanceState::NotAccepting(NotAcceptingReason::BlockProductionDisabled)
        ));
    }
}
