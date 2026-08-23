use crate::metrics::L1_STATE_METRICS;
use crate::models::BatchDaInputMode;
use crate::settlement_layer_intervals::{IntervalSettlementLayer, SettlementLayerIntervals};
use crate::{Bridgehub, MultisigCommitter, PubdataPricingMode, ZkChain};
use alloy::eips::BlockId;
use alloy::primitives::{Address, U256, address};
use alloy::providers::Provider;
use anyhow::Context;
use backon::{ConstantBuilder, Retryable};
use std::fmt::Debug;
use std::time::Duration;
use zksync_os_provider::NodeProvider;

/// Standard L2 bridgehub address — present at the same well-known address on every Gateway.
pub const L2_BRIDGEHUB_ADDRESS: Address = address!("0x0000000000000000000000000000000000010002");

#[derive(Clone, Debug)]
pub struct BatchVerificationSLConfig {
    pub threshold: u64,
    pub validators: Vec<Address>,
}

#[derive(Clone, Debug)]
pub enum BatchVerificationSL {
    Disabled,
    Enabled(BatchVerificationSLConfig),
}

#[derive(Clone, Debug)]
pub struct L1State {
    pub bridgehub_l1: Bridgehub<NodeProvider>,
    pub bridgehub_sl: Bridgehub<NodeProvider>,
    pub diamond_proxy_l1: ZkChain<NodeProvider>,
    pub diamond_proxy_sl: ZkChain<NodeProvider>,
    pub validator_timelock_sl: Address,
    pub batch_verification: BatchVerificationSL,
    pub last_committed_batch: u64,
    pub last_proved_batch: u64,
    pub last_executed_batch: u64,
    pub last_finalized_executed_batch: u64,
    /// Block number on SL that was used to query `last_committed_batch`, `last_proved_batch`, `last_executed_batch`.
    pub sl_block_number: u64,
    /// Finalized SL block number that was used to query `last_finalized_executed_batch`.
    pub finalized_sl_block_number: u64,
    pub da_input_mode: BatchDaInputMode,
    pub l1_chain_id: u64,
    pub sl_chain_id: u64,
    /// The address returned by `getSettlementLayer()` on the L1 diamond proxy at startup.
    /// `Address::ZERO` means the chain is settling on L1; any other address is the Gateway.
    pub settlement_layer_address: Address,
    /// Settlement layer intervals discovered on startup. Can be used to route batch lookups to the
    /// diamond proxy of the SL the batch was committed to.
    pub settlement_layer_intervals: SettlementLayerIntervals,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BatchFinality {
    last_committed_batch: u64,
    last_proved_batch: u64,
    last_executed_batch: u64,
}

impl L1State {
    /// Resolves the L1 diamond proxy for this chain via the L1 Bridgehub, without fetching any
    /// batch-finality state.
    ///
    /// Startup uses this to initialize genesis and the repository manager *before* the startup L1
    /// revert (the revert's `from_block_hash` guard reads the current local block hash via the
    /// repository manager). The full [`L1State`] — including the batch-finality numbers
    /// (`last_committed_batch`, ...) that a revert invalidates — is fetched only after the revert
    /// decision point, so no stale batch-finality data can reach the components initialized here.
    pub async fn resolve_diamond_proxy_l1(
        l1_provider: NodeProvider,
        bridgehub_address_l1: Address,
        l2_chain_id: u64,
    ) -> anyhow::Result<ZkChain<NodeProvider>> {
        let (_, diamond_proxy_l1) =
            Self::resolve_l1_bridgehub_and_proxy(l1_provider, bridgehub_address_l1, l2_chain_id)
                .await?;
        Ok(diamond_proxy_l1)
    }

    /// Builds the L1 Bridgehub handle and resolves the chain's L1 diamond proxy through it.
    async fn resolve_l1_bridgehub_and_proxy(
        l1_provider: NodeProvider,
        bridgehub_address_l1: Address,
        l2_chain_id: u64,
    ) -> anyhow::Result<(Bridgehub<NodeProvider>, ZkChain<NodeProvider>)> {
        let bridgehub_l1 = Bridgehub::new(bridgehub_address_l1, l1_provider, l2_chain_id);
        let diamond_proxy_l1 = bridgehub_l1.zk_chain().await?;
        Ok((bridgehub_l1, diamond_proxy_l1))
    }

    /// Fetches L1 ecosystem contracts along with batch finality status as of latest block.
    ///
    /// `gateway_provider` must be `Some` when the chain is currently settling on the Gateway
    /// (an error is returned if missing). It may also be passed when the chain is currently
    /// settling on L1 but has historical Gateway intervals — in that case the Gateway diamond
    /// proxy is resolved from it so historical batches committed on the Gateway can still be
    /// looked up through [`SettlementLayerIntervals`].
    pub async fn fetch(
        l1_provider: NodeProvider,
        gateway_provider: Option<NodeProvider>,
        bridgehub_address_l1: Address,
        l2_chain_id: u64,
    ) -> anyhow::Result<Self> {
        let l1_chain_id = l1_provider.get_chain_id().await?;

        let (bridgehub_l1, diamond_proxy_l1) =
            Self::resolve_l1_bridgehub_and_proxy(l1_provider, bridgehub_address_l1, l2_chain_id)
                .await?;

        // Call ZKChainStorage::getSettlementLayer() on the L1 diamond proxy to determine whether
        // this chain is currently settling on L1 or on the Gateway.
        // Returns address(0) when settling on L1, or the Gateway diamond proxy address after migration.
        let settlement_layer_address = diamond_proxy_l1
            .get_settlement_layer(BlockId::latest())
            .await?;
        let (sl_chain_id, bridgehub_sl) = if settlement_layer_address.is_zero() {
            // Settling on L1: the settlement layer is L1 itself.
            (l1_chain_id, bridgehub_l1.clone())
        } else {
            // Settling on Gateway: require a dedicated Gateway RPC provider.
            let gateway_provider = gateway_provider.as_ref().with_context(|| {
                format!(
                    "chain is settling on Gateway (settlement layer: {settlement_layer_address}) \
                     but no gateway RPC URL is configured"
                )
            })?;
            let sl_chain_id = gateway_provider.get_chain_id().await?;
            anyhow::ensure!(
                sl_chain_id != l1_chain_id,
                "settling on Gateway but SL chain ID is identical to L1 chain ID"
            );
            let bridgehub_sl =
                Bridgehub::new(L2_BRIDGEHUB_ADDRESS, gateway_provider.clone(), l2_chain_id);
            (sl_chain_id, bridgehub_sl)
        };

        // SYSCOIN: Bind the configured Gateway RPC identity to the Gateway proxy selected by the
        // canonical L1 chain. Chain-ID whitelisting alone is insufficient: another whitelisted
        // Gateway could expose this edge chain and otherwise pass all startup discovery checks.
        Self::validate_settlement_layer_identity(
            &bridgehub_l1,
            settlement_layer_address,
            sl_chain_id,
        )
        .await?;
        Self::validate_chain_ids(&bridgehub_l1, &bridgehub_sl, l2_chain_id).await?;

        let diamond_proxy_sl = bridgehub_sl.zk_chain().await?;
        let validator_timelock_sl = bridgehub_sl.validator_timelock_address().await?;

        // SYSCOIN: wait for a finalized SL block before sampling latest counters. Sampling latest
        // first can mix stale latest counters with a later finalized frontier during fresh startup.
        let (finalized_sl_block_number, last_finalized_executed_batch) =
            fetch_finalized_executed_batch(&diamond_proxy_sl).await?;
        let latest_sl_block_number = diamond_proxy_sl.provider().get_block_number().await?;
        let last_committed_batch = diamond_proxy_sl
            .get_total_batches_committed(latest_sl_block_number.into())
            .await?;
        let last_proved_batch = diamond_proxy_sl
            .get_total_batches_proved(latest_sl_block_number.into())
            .await?;
        let last_executed_batch = diamond_proxy_sl
            .get_total_batches_executed(latest_sl_block_number.into())
            .await?;
        // SYSCOIN: keep the finalized frontier from `fetch()`. Refetching it after
        // `wait_to_finalize()` can again combine a later finalized block with earlier latest counters.
        validate_batch_frontiers(
            last_committed_batch,
            last_proved_batch,
            last_executed_batch,
            last_finalized_executed_batch,
        )?;

        let pubdata_pricing_mode = diamond_proxy_sl.get_pubdata_pricing_mode().await?;
        let da_input_mode = match pubdata_pricing_mode {
            PubdataPricingMode::Rollup => BatchDaInputMode::Rollup,
            PubdataPricingMode::Validium => BatchDaInputMode::Validium,
            v => panic!("unexpected pubdata pricing mode: {}", v as u8),
        };

        let batch_verification = match MultisigCommitter::try_new(
            validator_timelock_sl,
            diamond_proxy_sl.provider().clone(),
            *diamond_proxy_sl.address(),
        )
        .await
        .context("failed to check MultisigCommitter interface")?
        {
            Some(multisig_committer) => {
                let threshold = multisig_committer
                    .get_signing_threshold()
                    .await
                    .context("failed to get signing threshold")?;
                let validators = multisig_committer
                    .get_validators()
                    .await
                    .context("failed to get validators")?;
                BatchVerificationSL::Enabled(BatchVerificationSLConfig {
                    threshold,
                    validators,
                })
            }
            None => BatchVerificationSL::Disabled,
        };

        let chain_asset_handler = bridgehub_l1.chain_asset_handler_address().await?;
        let settlement_layer_intervals = SettlementLayerIntervals::discover(
            chain_asset_handler,
            diamond_proxy_l1.clone(),
            gateway_provider,
            l2_chain_id,
        )
        .await?;
        let current_interval = settlement_layer_intervals
            .intervals()
            .last()
            .expect("settlement layer discovery always returns an open interval");
        // SYSCOIN: getSettlementLayer() and migration intervals are sampled by separate RPC
        // reads. Abort if a concurrent migration produced a split startup topology rather than
        // routing execution and settlement through different layers.
        Self::validate_settlement_topology_snapshot(
            settlement_layer_address,
            l1_chain_id,
            sl_chain_id,
            *diamond_proxy_l1.address(),
            *diamond_proxy_sl.address(),
            &current_interval.settlement_layer,
            *current_interval.proxy.address(),
            current_interval.last_batch,
        )?;
        tracing::info!(
            "discovered {} settlement layer intervals: {:?}",
            settlement_layer_intervals.intervals().len(),
            settlement_layer_intervals.intervals(),
        );

        Ok(Self {
            bridgehub_l1,
            bridgehub_sl,
            diamond_proxy_l1,
            diamond_proxy_sl,
            validator_timelock_sl,
            batch_verification,
            last_committed_batch,
            last_proved_batch,
            last_executed_batch,
            last_finalized_executed_batch,
            sl_block_number: latest_sl_block_number,
            finalized_sl_block_number,
            da_input_mode,
            l1_chain_id,
            sl_chain_id,
            settlement_layer_address,
            settlement_layer_intervals,
        })
    }

    async fn validate_chain_ids(
        bridgehub_l1: &Bridgehub<NodeProvider>,
        bridgehub_sl: &Bridgehub<NodeProvider>,
        l2_chain_id: u64,
    ) -> anyhow::Result<()> {
        let all_chain_ids_l1 = bridgehub_l1.get_all_zk_chain_chain_ids().await?;
        let all_chain_ids_sl = bridgehub_sl.get_all_zk_chain_chain_ids().await?;
        anyhow::ensure!(
            all_chain_ids_l1.contains(&U256::from(l2_chain_id)),
            "chain ID {l2_chain_id} is not registered on L1"
        );
        anyhow::ensure!(
            all_chain_ids_sl.contains(&U256::from(l2_chain_id)),
            "chain ID {l2_chain_id} is not registered on SL"
        );

        let sl_chain_id = bridgehub_sl.instance.provider().get_chain_id().await?;
        let is_sl_whitelisted = bridgehub_l1
            .whitelisted_settlement_layers(U256::from(sl_chain_id))
            .await?;
        anyhow::ensure!(is_sl_whitelisted, "SL is not whitelisted on L1 Bridgehub");

        Ok(())
    }

    /// SYSCOIN: Fails closed unless a nonzero `getSettlementLayer()` address is the L1
    /// Bridgehub's registered ZK chain for the configured Gateway RPC's chain ID. Direct-L1
    /// settlement has no Gateway identity to resolve and remains unaffected.
    async fn validate_settlement_layer_identity(
        bridgehub_l1: &Bridgehub<NodeProvider>,
        settlement_layer_address: Address,
        sl_chain_id: u64,
    ) -> anyhow::Result<()> {
        if settlement_layer_address.is_zero() {
            return Ok(());
        }

        let registered_gateway = bridgehub_l1
            .zk_chain_by_chain_id(sl_chain_id)
            .await
            .with_context(|| {
                format!("failed to resolve Gateway chain ID {sl_chain_id} through the L1 Bridgehub")
            })?;
        let registered_gateway_address = *registered_gateway.address();
        anyhow::ensure!(
            registered_gateway_address == settlement_layer_address,
            "Gateway settlement-layer identity mismatch: L1 getSettlementLayer() returned \
             {settlement_layer_address}, but L1 Bridgehub.getZKChain({sl_chain_id}) returned \
             {registered_gateway_address}"
        );
        Ok(())
    }

    /// SYSCOIN: Fails closed when independently sampled settlement-layer signals describe
    /// different current topologies, as can happen if migration races startup discovery.
    #[allow(clippy::too_many_arguments)]
    fn validate_settlement_topology_snapshot(
        settlement_layer_address: Address,
        l1_chain_id: u64,
        sl_chain_id: u64,
        diamond_proxy_l1: Address,
        diamond_proxy_sl: Address,
        interval_layer: &IntervalSettlementLayer,
        interval_proxy: Address,
        interval_last_batch: Option<u64>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            interval_last_batch.is_none(),
            "current settlement-layer interval is unexpectedly closed at batch {:?}",
            interval_last_batch
        );
        if settlement_layer_address.is_zero() {
            anyhow::ensure!(
                sl_chain_id == l1_chain_id,
                "direct-L1 settlement snapshot uses SL chain ID {sl_chain_id}, expected L1 chain ID {l1_chain_id}"
            );
            anyhow::ensure!(
                matches!(interval_layer, IntervalSettlementLayer::L1),
                "getSettlementLayer() reports direct L1 but the open interval reports {interval_layer}"
            );
            anyhow::ensure!(
                diamond_proxy_sl == diamond_proxy_l1 && interval_proxy == diamond_proxy_l1,
                "direct-L1 settlement proxy mismatch: L1={diamond_proxy_l1}, SL={diamond_proxy_sl}, interval={interval_proxy}"
            );
            return Ok(());
        }

        anyhow::ensure!(
            sl_chain_id != l1_chain_id,
            "Gateway settlement snapshot uses the L1 chain ID {l1_chain_id}"
        );
        anyhow::ensure!(
            matches!(interval_layer, IntervalSettlementLayer::Gateway(chain_id) if *chain_id == sl_chain_id),
            "getSettlementLayer() reports Gateway chain ID {sl_chain_id} but the open interval reports {interval_layer}"
        );
        anyhow::ensure!(
            interval_proxy == diamond_proxy_sl,
            "Gateway settlement proxy mismatch: resolved SL={diamond_proxy_sl}, interval={interval_proxy}"
        );
        Ok(())
    }

    /// Equivalent to [`Self::fetch`] that also waits until the pending SL state is consistent with the
    /// latest SL state (i.e., there are no pending transactions that are committing / proving /
    /// executing batches on the settlement layer).
    ///
    /// NOTE: This should only be called on the main node as ENs will observe pending changes that
    /// are being submitted by the main node.
    pub async fn fetch_finalized(
        l1_provider: NodeProvider,
        gateway_provider: Option<NodeProvider>,
        bridgehub_address: Address,
        chain_id: u64,
        // SYSCOIN: preserve the configured startup finalization wait used by the direct-settlement path.
        startup_sl_finalization_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let this = Self::fetch(l1_provider, gateway_provider, bridgehub_address, chain_id).await?;
        let zk_chain_sl = &this.diamond_proxy_sl;
        let (sl_block_number, batch_finality) = wait_to_finalize(
            zk_chain_sl.provider(),
            startup_sl_finalization_timeout,
            |block_id| async move {
                Ok(BatchFinality {
                    last_committed_batch: zk_chain_sl.get_total_batches_committed(block_id).await?,
                    last_proved_batch: zk_chain_sl.get_total_batches_proved(block_id).await?,
                    last_executed_batch: zk_chain_sl.get_total_batches_executed(block_id).await?,
                })
            },
        )
        .await
        .context("failed to fetch finalized batch state")?;
        validate_batch_frontiers(
            batch_finality.last_committed_batch,
            batch_finality.last_proved_batch,
            batch_finality.last_executed_batch,
            this.last_finalized_executed_batch,
        )?;
        Ok(Self {
            bridgehub_l1: this.bridgehub_l1,
            bridgehub_sl: this.bridgehub_sl,
            diamond_proxy_l1: this.diamond_proxy_l1,
            diamond_proxy_sl: this.diamond_proxy_sl,
            validator_timelock_sl: this.validator_timelock_sl,
            batch_verification: this.batch_verification,
            last_committed_batch: batch_finality.last_committed_batch,
            last_proved_batch: batch_finality.last_proved_batch,
            last_executed_batch: batch_finality.last_executed_batch,
            last_finalized_executed_batch: this.last_finalized_executed_batch,
            sl_block_number,
            finalized_sl_block_number: this.finalized_sl_block_number,
            da_input_mode: this.da_input_mode,
            l1_chain_id: this.l1_chain_id,
            sl_chain_id: this.sl_chain_id,
            settlement_layer_address: this.settlement_layer_address,
            settlement_layer_intervals: this.settlement_layer_intervals,
        })
    }

    /// Fetch L1 state, optionally waiting for all pending L1 transactions to finalize first.
    pub async fn fetch_with_finality(
        use_finalized: bool,
        l1_provider: NodeProvider,
        gateway_provider: Option<NodeProvider>,
        bridgehub_address: Address,
        chain_id: u64,
        startup_sl_finalization_timeout: Duration,
    ) -> anyhow::Result<Self> {
        if use_finalized {
            Self::fetch_finalized(
                l1_provider,
                gateway_provider,
                bridgehub_address,
                chain_id,
                startup_sl_finalization_timeout,
            )
            .await
        } else {
            Self::fetch(l1_provider, gateway_provider, bridgehub_address, chain_id).await
        }
    }

    pub fn diamond_proxy_address_sl(&self) -> Address {
        *self.diamond_proxy_sl.address()
    }

    /// `true` when the chain is currently committing batches to a Gateway, derived from the
    /// settlement layer interval discovered at startup.
    pub fn settles_on_gateway(&self) -> bool {
        self.settlement_layer_intervals.settles_on_gateway()
    }

    pub fn report_metrics(&self) {
        // Need to leak Strings here as metric exporter expects label names as `&'static`
        // This only happens once per process lifetime so is safe
        let bridgehub: &'static str = self.bridgehub_l1.address().to_string().leak();
        let diamond_proxy: &'static str = self.diamond_proxy_l1.address().to_string().leak();
        let validator_timelock: &'static str = self.validator_timelock_sl.to_string().leak();
        L1_STATE_METRICS.l1_contract_addresses[&(bridgehub, diamond_proxy, validator_timelock)]
            .set(1);

        let da_input_mode: &'static str = match self.da_input_mode {
            BatchDaInputMode::Rollup => "rollup",
            BatchDaInputMode::Validium => "validium",
        };
        L1_STATE_METRICS.da_input_mode[&da_input_mode].set(1);
    }
}

async fn fetch_finalized_executed_batch(
    zk_chain_sl: &ZkChain<NodeProvider>,
) -> anyhow::Result<(u64, u64)> {
    const RETRY_DELAY: Duration = Duration::from_secs(1);
    let retry_builder = ConstantBuilder::new()
        .with_delay(RETRY_DELAY)
        .without_max_times();
    let mut retries = 0_u64;
    let finalized_sl_block_number = (|| async {
        let block_number = zk_chain_sl
            .provider()
            .get_block_number_by_id(BlockId::finalized())
            .await
            .context("failed to fetch finalized SL block number");
        match block_number {
            Ok(Some(block_number)) => Ok(Ok(block_number)),
            Ok(None) => {
                // SYSCOIN: a fresh Gateway/SL may support the finalized tag before any block is
                // finalized. Wait for finality instead of aborting startup discovery.
                Err(())
            }
            Err(err) => Ok(Err(err)),
        }
    })
    .retry(retry_builder)
    .notify(|(), _| {
        retries = retries.saturating_add(1);
        if retries == 1 || retries.is_multiple_of(30) {
            tracing::warn!(retries, "finalized SL block is not available yet; waiting");
        }
    })
    .await;

    let finalized_sl_block_number = match finalized_sl_block_number {
        Ok(Ok(block_number)) => block_number,
        Ok(Err(err)) => return Err(err),
        Err(()) => {
            return Err(anyhow::anyhow!(
                "finalized SL block retry loop stopped unexpectedly"
            ));
        }
    };

    if !zk_chain_sl
        .code_exists_at_block(finalized_sl_block_number.into())
        .await
        .context("failed to check ZK chain contract code at finalized SL block")?
    {
        return Ok((finalized_sl_block_number, 0));
    }

    let last_finalized_executed_batch = zk_chain_sl
        .get_total_batches_executed(finalized_sl_block_number.into())
        .await?;
    Ok((finalized_sl_block_number, last_finalized_executed_batch))
}

fn validate_batch_frontiers(
    last_committed_batch: u64,
    last_proved_batch: u64,
    last_executed_batch: u64,
    last_finalized_executed_batch: u64,
) -> anyhow::Result<()> {
    // SYSCOIN: L1 startup state must be sampled from compatible SL frontiers. A finalized
    // frontier ahead of latest counters can make startup skip required committed batches.
    anyhow::ensure!(
        last_finalized_executed_batch <= last_executed_batch,
        "inconsistent L1 batch frontiers: finalized executed batch {} is ahead of executed batch {}",
        last_finalized_executed_batch,
        last_executed_batch
    );
    anyhow::ensure!(
        last_executed_batch <= last_proved_batch,
        "inconsistent L1 batch frontiers: executed batch {} is ahead of proved batch {}",
        last_executed_batch,
        last_proved_batch
    );
    anyhow::ensure!(
        last_proved_batch <= last_committed_batch,
        "inconsistent L1 batch frontiers: proved batch {} is ahead of committed batch {}",
        last_proved_batch,
        last_committed_batch
    );
    Ok(())
}

/// Waits until the pending SL state matches the latest SL block.
async fn wait_to_finalize<T: Debug + PartialEq, Fut: Future<Output = crate::Result<T>>>(
    provider: &NodeProvider,
    warning_interval: Duration,
    f: impl Fn(BlockId) -> Fut,
) -> anyhow::Result<(u64, T)> {
    /// SYSCOIN: We probe once per second so startup can proceed as soon as pending SL state finalizes.
    const RETRY_DELAY: Duration = Duration::from_secs(1);
    let retry_builder = ConstantBuilder::new()
        .with_delay(RETRY_DELAY)
        // SYSCOIN: pending settlement-layer transactions can stay pending longer than any fixed
        // startup budget. Keep the safety gate, but do not crash-loop the node while waiting.
        .without_max_times();
    let warning_interval_retries = warning_interval
        .as_millis()
        .div_ceil(RETRY_DELAY.as_millis())
        .max(1) as u64;

    let pending_value = f(BlockId::pending())
        .await
        .context("failed to get pending value")?;
    // Note: we do not retry networking errors here. We only retry if the pending state is ahead of latest
    // Outer `Result` is used for retries, inner result is propagated as is.
    let result = (|| async {
        let latest_block_number = provider
            .get_block_number()
            .await
            .context("failed to get latest block number");
        let latest_block_number = match latest_block_number {
            Ok(latest_block_number) => latest_block_number,
            Err(err) => return Ok(Err(err)),
        };
        let last_value = f(latest_block_number.into())
            .await
            .context("failed to get latest value");
        match last_value {
            Ok(last_value) if last_value == pending_value => {
                Ok(Ok((latest_block_number, last_value)))
            }
            Ok(last_value) => Err((latest_block_number, last_value)),
            Err(err) => Ok(Err(err)),
        }
    })
    .retry(retry_builder)
    .notify({
        let pending_value = &pending_value;
        let mut retries = 0_u64;
        let mut next_warning_retry = 1_u64;
        move |(latest_block_number, last_value), _| {
            retries = retries.saturating_add(1);
            if retries >= next_warning_retry {
                next_warning_retry = retries.saturating_add(warning_interval_retries);
                tracing::warn!(
                    pending_value = ?pending_value,
                    latest_block_number,
                    latest_value = ?last_value,
                    "encountered a pending SL state change; waiting for it to finalize"
                );
            } else {
                tracing::debug!(
                    pending_value = ?pending_value,
                    latest_block_number,
                    latest_value = ?last_value,
                    "encountered a pending SL state change; waiting for it to finalize"
                );
            }
        }
    })
    .await;

    match result {
        Ok(last_result) => {
            let (latest_block_number, last_value) = last_result?;
            // Sanity-check that the pending state has not changed since we started waiting.
            let pending_value = f(BlockId::pending())
                .await
                .context("failed to get pending value")?;
            if pending_value != last_value {
                Err(anyhow::anyhow!(
                    "pending state changed while waiting for it to finalize; another main node could already be running"
                ))
            } else {
                Ok((latest_block_number, last_value))
            }
        }
        Err((latest_block_number, last_value)) => Err(anyhow::anyhow!(
            "pending state finalization retry loop stopped unexpectedly at SL block {latest_block_number}; last value: {last_value:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::network::EthereumWallet;
    use alloy::primitives::{Bytes, address};
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::types::Header;
    use alloy::sol_types::SolValue;
    use alloy::transports::mock::Asserter;

    const EDGE_CHAIN_ID: u64 = 506;
    const GATEWAY_CHAIN_ID: u64 = 270;

    fn header_with_number(number: u64) -> Header<alloy::consensus::Header> {
        let mut header: Header<alloy::consensus::Header> = Header::default();
        header.inner.number = number;
        header
    }

    async fn mocked_bridgehub_l1(asserter: &Asserter) -> Bridgehub<NodeProvider> {
        // NodeProvider capability probes: latest header and finalized header.
        asserter.push_success(&header_with_number(1));
        asserter.push_success(&header_with_number(1));
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        let provider = NodeProvider::new(provider)
            .await
            .expect("mocked provider construction should succeed");
        Bridgehub::new(
            address!("0x0000000000000000000000000000000000001002"),
            provider,
            EDGE_CHAIN_ID,
        )
    }

    #[test]
    fn batch_frontiers_allow_ordered_state() {
        validate_batch_frontiers(10, 9, 8, 7).expect("ordered frontiers must be accepted");
        validate_batch_frontiers(0, 0, 0, 0).expect("empty fresh-chain frontiers must be accepted");
    }

    #[test]
    fn batch_frontiers_reject_finalized_ahead_of_latest() {
        let err = validate_batch_frontiers(1, 1, 1, 2)
            .expect_err("finalized executed cannot be ahead of latest executed");
        assert!(
            err.to_string().contains("finalized executed batch 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn batch_frontiers_reject_non_monotonic_latest_counters() {
        assert!(validate_batch_frontiers(1, 1, 2, 1).is_err());
        assert!(validate_batch_frontiers(1, 2, 1, 1).is_err());
    }

    // SYSCOIN: A configured Gateway must be the exact ZK-chain proxy selected on canonical L1.
    #[tokio::test]
    async fn settlement_layer_identity_accepts_registered_gateway() {
        let asserter = Asserter::new();
        let bridgehub_l1 = mocked_bridgehub_l1(&asserter).await;
        let gateway = address!("0x0000000000000000000000000000000000002002");
        asserter.push_success(&Bytes::from(gateway.abi_encode()));

        L1State::validate_settlement_layer_identity(&bridgehub_l1, gateway, GATEWAY_CHAIN_ID)
            .await
            .expect("matching Gateway identity must be accepted");

        assert!(asserter.read_q().is_empty(), "all responses consumed");
    }

    // SYSCOIN: Whitelisting a Gateway chain ID must not let a different Gateway RPC identity pass.
    #[tokio::test]
    async fn settlement_layer_identity_rejects_different_registered_gateway() {
        let asserter = Asserter::new();
        let bridgehub_l1 = mocked_bridgehub_l1(&asserter).await;
        let selected_gateway = address!("0x0000000000000000000000000000000000002002");
        let registered_gateway = address!("0x0000000000000000000000000000000000003003");
        asserter.push_success(&Bytes::from(registered_gateway.abi_encode()));

        let err = L1State::validate_settlement_layer_identity(
            &bridgehub_l1,
            selected_gateway,
            GATEWAY_CHAIN_ID,
        )
        .await
        .expect_err("mismatched Gateway identity must be rejected");

        assert!(
            err.to_string().contains("identity mismatch"),
            "unexpected error: {err:#}"
        );
        assert!(asserter.read_q().is_empty(), "all responses consumed");
    }

    // SYSCOIN: Direct-L1 settlement must not perform a Gateway lookup.
    #[tokio::test]
    async fn settlement_layer_identity_leaves_direct_l1_unaffected() {
        let asserter = Asserter::new();
        let bridgehub_l1 = mocked_bridgehub_l1(&asserter).await;

        L1State::validate_settlement_layer_identity(&bridgehub_l1, Address::ZERO, GATEWAY_CHAIN_ID)
            .await
            .expect("direct-L1 settlement must not require a Gateway identity");

        assert!(
            asserter.read_q().is_empty(),
            "direct-L1 validation must not issue an RPC call"
        );
    }

    #[test]
    fn settlement_topology_snapshot_accepts_direct_l1() {
        let proxy = address!("0x0000000000000000000000000000000000004004");
        L1State::validate_settlement_topology_snapshot(
            Address::ZERO,
            57,
            57,
            proxy,
            proxy,
            &IntervalSettlementLayer::L1,
            proxy,
            None,
        )
        .expect("coherent direct-L1 snapshot must pass");
    }

    #[test]
    fn settlement_topology_snapshot_accepts_gateway() {
        let l1_proxy = address!("0x0000000000000000000000000000000000004004");
        let sl_proxy = address!("0x0000000000000000000000000000000000005005");
        L1State::validate_settlement_topology_snapshot(
            address!("0x0000000000000000000000000000000000006006"),
            57,
            57_001,
            l1_proxy,
            sl_proxy,
            &IntervalSettlementLayer::Gateway(57_001),
            sl_proxy,
            None,
        )
        .expect("coherent Gateway snapshot must pass");
    }

    #[test]
    fn settlement_topology_snapshot_rejects_split_layer_signals() {
        let proxy = address!("0x0000000000000000000000000000000000004004");
        assert!(
            L1State::validate_settlement_topology_snapshot(
                Address::ZERO,
                57,
                57,
                proxy,
                proxy,
                &IntervalSettlementLayer::Gateway(57_001),
                proxy,
                None,
            )
            .is_err()
        );
        assert!(
            L1State::validate_settlement_topology_snapshot(
                address!("0x0000000000000000000000000000000000006006"),
                57,
                57_001,
                proxy,
                proxy,
                &IntervalSettlementLayer::L1,
                proxy,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn settlement_topology_snapshot_rejects_gateway_id_or_proxy_mismatch() {
        let l1_proxy = address!("0x0000000000000000000000000000000000004004");
        let sl_proxy = address!("0x0000000000000000000000000000000000005005");
        let gateway = address!("0x0000000000000000000000000000000000006006");
        assert!(
            L1State::validate_settlement_topology_snapshot(
                gateway,
                57,
                57_001,
                l1_proxy,
                sl_proxy,
                &IntervalSettlementLayer::Gateway(57_002),
                sl_proxy,
                None,
            )
            .is_err()
        );
        assert!(
            L1State::validate_settlement_topology_snapshot(
                gateway,
                57,
                57_001,
                l1_proxy,
                sl_proxy,
                &IntervalSettlementLayer::Gateway(57_001),
                l1_proxy,
                None,
            )
            .is_err()
        );
    }
}
