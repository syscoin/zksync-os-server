use crate::contracts::L2InteropRootStorage;
use alloy::eips::BlockId;
use alloy::network::{Ethereum, Network, ReceiptResponse};
use alloy::primitives::B256;
use alloy::providers::ext::DebugApi;
use alloy::providers::{
    EthCall, PendingTransaction, PendingTransactionBuilder, Provider, RootProvider,
};
use alloy::rpc::json_rpc::RpcRecv;
use alloy::rpc::types::TransactionReceipt;
use alloy::rpc::types::trace::geth::{CallConfig, CallFrame, GethDebugTracingOptions};
use anyhow::Context;
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

// SYSCOIN: A failed receipt cannot become a success because optional diagnostics are unavailable.
async fn require_successful_receipt<N: Network>(
    receipt: N::ReceiptResponse,
    provider: &RootProvider<N>,
) -> anyhow::Result<N::ReceiptResponse> {
    if !receipt.status() {
        tracing::error!(?receipt, "Transaction failed");
        // SYSCOIN: Diagnostics are optional; receipt status remains authoritative.
        if let Ok(trace) = provider
            .debug_trace_transaction(
                receipt.transaction_hash(),
                GethDebugTracingOptions::call_tracer(CallConfig::default()),
            )
            .await
            && let Ok(call_frame) = trace.try_into_call_frame()
        {
            tracing::error!(?call_frame, "Failed call frame");
        }
        anyhow::bail!("transaction failed when it was expected to succeed");
    }
    Ok(receipt)
}

#[allow(async_fn_in_trait)]
pub trait EthCallAssert {
    async fn expect_to_fail(self, msg: &str);
}

impl<Resp: RpcRecv> EthCallAssert for EthCall<Ethereum, Resp> {
    async fn expect_to_fail(self, msg: &str) {
        let err = self
            .await
            .expect_err(&format!("`eth_call` should fail with error: {msg}"));
        assert!(
            err.to_string().contains(msg),
            "expected `eth_call` to fail with error '{msg}' but got: {err}",
        );
    }
}

#[allow(async_fn_in_trait)]
pub trait ReceiptAssert<N: Network> {
    async fn expect_successful_receipt(self) -> anyhow::Result<N::ReceiptResponse>;
    async fn expect_register(self) -> anyhow::Result<PendingTransaction>;
    async fn expect_to_execute(self) -> anyhow::Result<N::ReceiptResponse>;
    async fn expect_call_trace(self) -> anyhow::Result<CallFrame>;
}

impl<N: Network> ReceiptAssert<N> for PendingTransactionBuilder<N> {
    async fn expect_successful_receipt(self) -> anyhow::Result<N::ReceiptResponse> {
        let provider = self.provider().clone();
        let receipt = self
            .with_timeout(Some(DEFAULT_TIMEOUT))
            .get_receipt()
            .await?;
        require_successful_receipt(receipt, &provider).await
    }

    async fn expect_register(self) -> anyhow::Result<PendingTransaction> {
        Ok(self.with_timeout(Some(DEFAULT_TIMEOUT)).register().await?)
    }

    async fn expect_to_execute(self) -> anyhow::Result<N::ReceiptResponse> {
        let provider = self.provider().clone();
        let receipt = self.expect_successful_receipt().await?;
        let expected_block = receipt
            .block_number()
            .context("mined receipt is missing block number")?;
        // Wait until the expected block is executed.
        let mut retries = DEFAULT_TIMEOUT.div_duration_f64(POLL_INTERVAL).floor() as u64;
        while retries > 0 {
            // Finalized block is mapped to the latest executed block.
            let executed_block = provider
                .get_block_number_by_id(BlockId::finalized())
                .await?
                .unwrap_or(0);
            if executed_block >= expected_block {
                tracing::debug!(executed_block, "expected block was executed");
                return Ok(receipt);
            } else {
                tracing::debug!(
                    executed_block,
                    expected_block,
                    "expected block was not executed yet, retrying..."
                );
                retries -= 1;
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        Err(anyhow::anyhow!(
            "transaction was not executed on L1 in time"
        ))
    }

    async fn expect_call_trace(self) -> anyhow::Result<CallFrame> {
        let provider = self.provider().clone();
        let receipt = self
            .with_timeout(Some(DEFAULT_TIMEOUT))
            .get_receipt()
            .await?;
        let trace = provider
            .debug_trace_transaction(
                receipt.transaction_hash(),
                GethDebugTracingOptions::call_tracer(CallConfig::default()),
            )
            .await?;
        trace
            .try_into_call_frame()
            .context("failed to parse call trace")
    }
}

#[allow(async_fn_in_trait)]
pub trait ReceiptsAssert {
    async fn expect_successful_receipts(self) -> anyhow::Result<Vec<TransactionReceipt>>;
}

impl ReceiptsAssert for Vec<PendingTransactionBuilder<Ethereum>> {
    async fn expect_successful_receipts(self) -> anyhow::Result<Vec<TransactionReceipt>> {
        let receipts =
            futures::future::join_all(self.into_iter().map(|tx| tx.expect_successful_receipt()))
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;
        Ok(receipts)
    }
}

#[allow(async_fn_in_trait)]
pub trait ProviderAssert {
    async fn expect_interop_root_inclusion(
        &self,
        chain_id: u64,
        batch_number: u64,
    ) -> anyhow::Result<B256>;
}

impl<P: Provider<Ethereum>> ProviderAssert for P {
    async fn expect_interop_root_inclusion(
        &self,
        chain_id: u64,
        batch_number: u64,
    ) -> anyhow::Result<B256> {
        let root_storage = L2InteropRootStorage::new(&self);
        // Wait until the expected block is executed.
        let mut retries = DEFAULT_TIMEOUT.div_duration_f64(POLL_INTERVAL).floor() as u64;
        while retries > 0 {
            let root = root_storage
                .get_interop_root(chain_id, batch_number)
                .await?;
            if root.is_zero() {
                tracing::info!(chain_id, batch_number, "interop root not included yet");
                retries -= 1;
                tokio::time::sleep(POLL_INTERVAL).await;
            } else {
                tracing::info!(chain_id, batch_number, ?root, "interop root was included");
                return Ok(root);
            }
        }
        Err(anyhow::anyhow!("interop root not included on time"))
    }
}

#[cfg(test)]
mod tests {
    // SYSCOIN: Bounded receipt-status controls do not depend on a live chain or debug namespace.
    use super::*;
    use alloy::providers::ProviderBuilder;
    use alloy::transports::mock::Asserter;

    fn receipt(success: bool) -> TransactionReceipt {
        serde_json::from_value(serde_json::json!({
            "transactionHash": B256::ZERO,
            "from": alloy::primitives::Address::ZERO,
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x0",
            "cumulativeGasUsed": "0x5208",
            "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "status": if success { "0x1" } else { "0x0" },
            "type": "0x0"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn failed_receipt_still_fails_when_trace_rpc_is_unavailable() {
        let responses = Asserter::new();
        responses.push_failure_msg("debug namespace disabled");
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_mocked_client(responses);
        let error = require_successful_receipt::<Ethereum>(receipt(false), &provider)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("transaction failed"));
    }

    #[tokio::test]
    async fn failed_receipt_still_fails_when_optional_trace_is_not_a_call_frame() {
        let responses = Asserter::new();
        responses.push_success(&serde_json::json!({"notACallFrame": true}));
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_mocked_client(responses);
        assert!(
            require_successful_receipt::<Ethereum>(receipt(false), &provider)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn successful_receipt_does_not_require_debug_rpc() {
        let responses = Asserter::new();
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_mocked_client(responses);
        let receipt = require_successful_receipt::<Ethereum>(receipt(true), &provider)
            .await
            .unwrap();
        assert!(receipt.status());
    }
}
