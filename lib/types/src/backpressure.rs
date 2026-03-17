//! Global backpressure handle.
//!
//! Pipeline components signal overload by calling [`BackpressureHandle::set_overloaded`].
//! When cleared the handle broadcasts [`TransactionAcceptanceState::Accepting`] again.
//!
//! Multiple simultaneous causes are tracked; the node re-accepts only when **all** causes clear.

use crate::transaction_acceptance_state::{NotAcceptingReason, TransactionAcceptanceState};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

/// A unique backpressure cause identifying the component that is overloaded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackpressureCause {
    pub component: &'static str,
}

/// Global handle for broadcasting pipeline backpressure to the RPC layer.
pub struct BackpressureHandle {
    causes: Mutex<HashSet<BackpressureCause>>,
    state_tx: watch::Sender<TransactionAcceptanceState>,
}

impl BackpressureHandle {
    /// Create a standalone instance for testing (not the global singleton).
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let (tx, _rx) = watch::channel(TransactionAcceptanceState::Accepting);
        BackpressureHandle {
            causes: Mutex::new(HashSet::new()),
            state_tx: tx,
        }
    }
}

impl BackpressureHandle {
    /// Returns the process-global singleton.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<BackpressureHandle> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let (tx, _rx) = watch::channel(TransactionAcceptanceState::Accepting);
            BackpressureHandle {
                causes: Mutex::new(HashSet::new()),
                state_tx: tx,
            }
        })
    }

    /// Subscribe to acceptance-state changes.
    pub fn subscribe(&self) -> watch::Receiver<TransactionAcceptanceState> {
        self.state_tx.subscribe()
    }

    /// Borrow the current acceptance state without holding a lock.
    pub fn borrow(&self) -> watch::Ref<'_, TransactionAcceptanceState> {
        self.state_tx.borrow()
    }

    /// Signal that `component` is backpressured.
    /// Broadcasts `NotAccepting` immediately.
    pub fn set_overloaded(&self, component: &'static str, retry_after_ms: u64) {
        let mut causes = self.causes.lock().expect("backpressure mutex poisoned");
        let cause = BackpressureCause { component };
        let is_new = causes.insert(cause);
        if is_new {
            tracing::warn!(
                component,
                retry_after_ms,
                "Pipeline backpressure detected — rejecting new transactions",
            );
            let _ = self.state_tx.send(TransactionAcceptanceState::NotAccepting(
                NotAcceptingReason::ComponentBackpressured {
                    component,
                    retry_after_ms,
                },
            ));
        }
    }

    /// Clear the backpressure signal for `component`.
    /// If no other causes remain, broadcasts `Accepting`.
    pub fn clear_overloaded(&self, component: &'static str) {
        let mut causes = self.causes.lock().expect("backpressure mutex poisoned");
        let removed = causes.remove(&BackpressureCause { component });
        if removed && causes.is_empty() {
            tracing::info!(
                component,
                "Pipeline backpressure cleared — accepting transactions again",
            );
            let _ = self.state_tx.send(TransactionAcceptanceState::Accepting);
        }
    }
}
