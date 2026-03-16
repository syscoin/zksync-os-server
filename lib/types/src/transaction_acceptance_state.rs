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
    /// The node is temporarily overloaded; client should retry after `retry_after_ms`.
    #[error("Node is temporarily overloaded ({cause}). Retry after {retry_after_ms}ms.")]
    Overloaded {
        cause: OverloadCause,
        retry_after_ms: u64,
    },
}

/// The specific pipeline component that triggered the overload condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum OverloadCause {
    #[error("prover queue is full")]
    ProverQueueFull,
    #[error("pipeline is saturated")]
    PipelineSaturated,
}

impl OverloadCause {
    /// The string used in JSON-RPC `data` payloads and Prometheus labels.
    pub fn as_rpc_str(self) -> &'static str {
        match self {
            Self::ProverQueueFull => "prover_queue_full",
            Self::PipelineSaturated => "pipeline_saturated",
        }
    }
}

impl NotAcceptingReason {
    /// Returns a structured JSON payload for the JSON-RPC `data` field.
    pub fn to_rpc_data(&self) -> serde_json::Value {
        match self {
            Self::BlockProductionDisabled => {
                serde_json::json!({ "reason": "block_production_disabled" })
            }
            Self::Overloaded {
                cause,
                retry_after_ms,
            } => serde_json::json!({
                "reason": cause.as_rpc_str(),
                "retry_after_ms": retry_after_ms,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overloaded_reason_display() {
        let reason = NotAcceptingReason::Overloaded {
            cause: OverloadCause::ProverQueueFull,
            retry_after_ms: 5_000,
        };
        assert!(reason.to_string().contains("prover"));
        assert!(reason.to_string().contains("5000"));
    }

    #[test]
    fn overloaded_is_copy() {
        let reason = NotAcceptingReason::Overloaded {
            cause: OverloadCause::PipelineSaturated,
            retry_after_ms: 1_000,
        };
        let _copy = reason;
        let _copy2 = reason; // would fail to compile if not Copy
    }
}
