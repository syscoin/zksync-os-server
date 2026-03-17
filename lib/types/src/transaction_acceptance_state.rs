/// Whether the node should be accepting transactions
#[derive(Debug, Clone)]
pub enum TransactionAcceptanceState {
    Accepting,
    NotAccepting(NotAcceptingReason),
}

/// Reason why the node is not accepting transactions
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum NotAcceptingReason {
    /// Block production has been disabled via config (`sequencer_max_blocks_to_produce`)
    #[error("Node is not currently accepting transactions: block production disabled.")]
    BlockProductionDisabled,
    /// A pipeline component is backpressured and the node is shedding load
    #[error(
        "Node is not currently accepting transactions: component '{component}' is backpressured."
    )]
    ComponentBackpressured {
        /// Name of the component that caused the overload
        component: &'static str,
        /// Suggested retry delay in milliseconds
        retry_after_ms: u64,
    },
}
