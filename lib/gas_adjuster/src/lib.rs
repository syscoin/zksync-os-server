//! This module determines the fees to pay in txs containing blocks submitted to the L1.

use crate::statistics::{GasStatistics, Statistics};
use alloy::consensus::{BlobTransactionSidecar, SidecarCoder, SimpleCoder};
use alloy::eips::BlockNumberOrTag;
use alloy::eips::eip4844::FIELD_ELEMENTS_PER_BLOB;
use alloy::primitives::{U64, U256};
use alloy::providers::{DynProvider, Provider};
use anyhow::Context;
use bitcoin_da_client::SyscoinClient;
use metrics::METRICS;
use num::rational::Ratio;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::watch;
use url::Url;
use zksync_os_rpc_api::types::L2FeeHistory;
use zksync_os_types::PubdataMode;

mod metrics;
mod statistics;

/// This component keeps track of the median `base_fee` from the last `max_base_fee_samples` blocks.
///
/// It also tracks the median `blob_base_fee` from the last `max_blob_base_fee_sample` blocks.
/// It is used to adjust the base_fee of transactions sent to L1.
#[derive(Debug)]
pub struct GasAdjuster {
    base_fee_statistics: GasStatistics<u128>,
    blob_base_fee_statistics: GasStatistics<u128>,
    gw_pubdata_price_statistics: GasStatistics<U256>,
    blob_fill_ratio_statistics: Statistics<Ratio<u64>>,

    config: GasAdjusterConfig,
    sl_provider: DynProvider,
    pubdata_price_sender: watch::Sender<Option<U256>>,
    blob_fill_ratio_sender: watch::Sender<Option<Ratio<u64>>>,
    sidecar_receiver: Receiver<BlobTransactionSidecar>,
}

#[derive(Debug)]
pub struct GasAdjusterConfig {
    pub pubdata_mode: PubdataMode,
    pub max_base_fee_samples: usize,
    pub num_samples_for_blob_base_fee_estimate: usize,
    pub max_blob_fill_ratio_samples: usize,
    pub max_priority_fee_per_gas: u128,
    pub poll_period: Duration,
    pub pubdata_pricing_multiplier: f64,
    // SYSCOIN
    pub bitcoin_da_rpc_url: Option<String>,
    pub bitcoin_da_rpc_user: Option<String>,
    pub bitcoin_da_rpc_password: Option<String>,
    pub bitcoin_da_poda_url: String,
    pub bitcoin_da_wallet_name: String,
    pub bitcoin_da_request_timeout: Duration,
    pub bitcoin_da_fee_conf_target: u16,
}

impl GasAdjuster {
    pub async fn new(
        sl_provider: DynProvider,
        config: GasAdjusterConfig,
        pubdata_price_sender: watch::Sender<Option<U256>>,
        blob_fill_ratio_sender: watch::Sender<Option<Ratio<u64>>>,
        sidecar_receiver: Receiver<BlobTransactionSidecar>,
    ) -> anyhow::Result<Self> {
        // Subtracting 1 from the "latest" block number to prevent errors in case
        // the info about the latest block is not yet present on the node.
        // This sometimes happens on Infura.
        let (current_block, fee_history) =
            Self::initial_base_fee_history(&sl_provider, &config).await?;

        let base_fee_statistics = GasStatistics::new(
            config.max_base_fee_samples,
            current_block,
            fee_history.iter().map(|fee| fee.base_fee_per_gas),
        );

        let blob_base_fee_statistics = GasStatistics::new(
            config.num_samples_for_blob_base_fee_estimate,
            current_block,
            fee_history.iter().map(|fee| fee.base_fee_per_blob_gas),
        );

        let gw_pubdata_price_statistics = GasStatistics::new(
            config.max_base_fee_samples,
            current_block,
            fee_history
                .iter()
                .filter_map(|fee| fee.pubdata_price_per_byte),
        );

        let this = Self {
            base_fee_statistics,
            blob_base_fee_statistics,
            gw_pubdata_price_statistics,
            blob_fill_ratio_statistics: Statistics::new(config.max_blob_fill_ratio_samples),
            config,
            sl_provider,
            pubdata_price_sender,
            blob_fill_ratio_sender,
            sidecar_receiver,
        };
        this.pubdata_price_sender
            .send_replace(Some(this.pubdata_price()));

        Ok(this)
    }

    /// Performs an actualization routine for `GasAdjuster`.
    /// This method is intended to be invoked periodically.
    pub async fn update_fees(&mut self) -> anyhow::Result<()> {
        // Subtracting 1 from the "latest" block number to prevent errors in case
        // the info about the latest block is not yet present on the node.
        // This sometimes happens on Infura.
        let current_block = self.sl_provider.get_block_number().await?.saturating_sub(1);

        let last_processed_block = self.base_fee_statistics.last_processed_block();

        if current_block > last_processed_block {
            let n_blocks = current_block - last_processed_block;
            // SYSCOIN: fetch the authoritative Syscoin DA fee before requesting any
            // backfilled fee history. If the RPC fails, this tick must not repeatedly
            // allocate/fetch growing L1 fee-history ranges.
            let fixed_blob_base_fee = if Self::uses_syscoin_blob_da(self.config.pubdata_mode) {
                Some(Self::bitcoin_blob_base_fee(&self.config).await?)
            } else {
                None
            };
            let fee_data =
                Self::base_fee_history(&self.sl_provider, current_block, n_blocks, None).await?;

            // SYSCOIN: only build samples after all fallible fetches have succeeded, so a
            // failed tick leaves all fee windows unchanged.
            let blob_base_fee_samples = if let Some(fixed_blob_base_fee) = fixed_blob_base_fee {
                vec![fixed_blob_base_fee; fee_data.len()]
            } else {
                fee_data
                    .iter()
                    .map(|fee| fee.base_fee_per_blob_gas)
                    .collect()
            };

            // We shouldn't rely on provider to return consistent results, so we check that we have at least one new sample.
            if let Some(current_base_fee_per_gas) = fee_data.last().map(|fee| fee.base_fee_per_gas)
            {
                if current_base_fee_per_gas > u64::MAX as u128 {
                    tracing::info!(
                        "Failed to report current_base_fee_per_gas = {current_base_fee_per_gas}, it exceeds u64::MAX"
                    );
                } else {
                    METRICS
                        .current_base_fee_per_gas
                        .set(current_base_fee_per_gas as u64);
                }
            }
            self.base_fee_statistics
                .add_samples(fee_data.iter().map(|fee| fee.base_fee_per_gas));
            if self.base_fee_statistics.median() <= u64::MAX as u128 {
                METRICS
                    .median_base_fee_per_gas
                    .set(self.base_fee_statistics.median() as u64);
            }

            if let Some(&current_blob_base_fee) = blob_base_fee_samples.last() {
                if current_blob_base_fee > u64::MAX as u128 {
                    tracing::info!(
                        "Failed to report current_blob_base_fee = {current_blob_base_fee}, it exceeds u64::MAX"
                    );
                } else {
                    METRICS
                        .current_blob_base_fee
                        .set(current_blob_base_fee as u64);
                }
            }
            self.blob_base_fee_statistics
                .add_samples(blob_base_fee_samples);
            if self.blob_base_fee_statistics.median() <= u64::MAX as u128 {
                METRICS
                    .median_blob_base_fee
                    .set(self.blob_base_fee_statistics.median() as u64);
            }

            if let Some(current_pubdata_price_per_byte) =
                fee_data.last().and_then(|fee| fee.pubdata_price_per_byte)
            {
                if current_pubdata_price_per_byte > U256::from(u64::MAX) {
                    tracing::info!(
                        "Failed to report current_pubdata_price_per_byte = {current_pubdata_price_per_byte}, it exceeds u64::MAX"
                    );
                } else {
                    METRICS
                        .current_pubdata_price_per_byte
                        .set(current_pubdata_price_per_byte.to());
                }
            }
            self.gw_pubdata_price_statistics
                .add_samples(fee_data.iter().filter_map(|fee| fee.pubdata_price_per_byte));
            if self.gw_pubdata_price_statistics.median() <= U256::from(u64::MAX) {
                METRICS
                    .median_pubdata_price_per_byte
                    .set(self.gw_pubdata_price_statistics.median().to());
            }

            self.pubdata_price_sender
                .send_replace(Some(self.pubdata_price()));
        }
        Ok(())
    }

    pub async fn update_blob_fill_ratios(&mut self) -> anyhow::Result<()> {
        loop {
            match self.sidecar_receiver.try_recv() {
                Ok(sidecar) => {
                    let mut decoder = SimpleCoder::default();
                    if let Some(decoded) = decoder.decode_all(&sidecar.blobs) {
                        if decoded.len() != 1 {
                            anyhow::bail!("Expected exactly one blob in sidecar");
                        }
                        let pubdata_len = decoded[0].len() as u64;
                        let total_size =
                            (FIELD_ELEMENTS_PER_BLOB * (sidecar.blobs.len() as u64) - 1) * 31;
                        self.blob_fill_ratio_statistics
                            .add_samples([Ratio::new(pubdata_len, total_size)]);

                        self.blob_fill_ratio_sender
                            .send_replace(self.blob_fill_ratio_median());
                    } else {
                        anyhow::bail!("Failed to decode blobs from sidecar");
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break Ok(()),
            }
        }
    }

    pub async fn run(mut self) {
        let mut timer = tokio::time::interval(self.config.poll_period);
        let mut attempts_failed_in_a_row = 0usize;
        loop {
            if let Err(err) = self.update_fees().await {
                attempts_failed_in_a_row += 1;
                if attempts_failed_in_a_row >= 5 {
                    tracing::warn!(
                        attempts_failed_in_a_row,
                        "Cannot add the base fee to gas statistics: {err}"
                    );
                }
            } else {
                attempts_failed_in_a_row = 0;
            }

            // `update_blob_fill_ratios` cannot fail due to transient issue, unlike `update_fees`.
            // So we log all errors.
            if let Err(err) = self.update_blob_fill_ratios().await {
                tracing::warn!("Cannot update blob fill ratios: {err}");
            }
            timer.tick().await;
        }
    }

    pub fn gas_price(&self) -> u128 {
        let median = self.base_fee_statistics.median();
        median + self.config.max_priority_fee_per_gas
    }

    pub fn pubdata_price(&self) -> U256 {
        let price = match self.config.pubdata_mode {
            // SYSCOIN: Gateway-settled child chains use `RelayedL2Calldata`, but their
            // pubdata is still published to Syscoin DA as compact blob references.
            PubdataMode::Blobs | PubdataMode::RelayedL2Calldata => {
                const BLOB_GAS_PER_BYTE: u128 = 1; // `BYTES_PER_BLOB` = `GAS_PER_BLOB` = 2 ^ 17.

                let blob_base_fee_median = self.blob_base_fee_statistics.median();
                U256::from(blob_base_fee_median * BLOB_GAS_PER_BYTE)
            }
            PubdataMode::Calldata => {
                /// The amount of gas we need to pay for each non-zero pubdata byte.
                /// Note that it is bigger than 16 to account for potential overhead.
                const L1_GAS_PER_PUBDATA_BYTE: u32 = 17;

                U256::from(self.gas_price()).saturating_mul(U256::from(L1_GAS_PER_PUBDATA_BYTE))
            }
            PubdataMode::Validium => U256::from(0u32),
        };

        if price <= U256::from(u128::MAX) {
            let price_u128: u128 = price.to();
            U256::from((self.config.pubdata_pricing_multiplier * price_u128 as f64) as u128)
        } else {
            tracing::info!(
                "`pubdata_pricing_multiplier` is not applied, as the price exceeds u128::MAX"
            );
            price
        }
    }

    pub fn blob_fill_ratio_median(&self) -> Option<Ratio<u64>> {
        self.blob_fill_ratio_statistics.median()
    }

    /// Collects the base fee history for the specified block range.
    ///
    /// Returns 1 value for each block in range, assuming that these blocks exist.
    /// Will return an error if the `upto_block` is beyond the head block.
    async fn base_fee_history(
        provider: &DynProvider,
        upto_block: u64,
        block_count: u64,
        fixed_blob_base_fee: Option<u128>,
    ) -> anyhow::Result<Vec<BaseFees>> {
        const FEE_HISTORY_MAX_REQUEST_CHUNK: usize = 1023;

        let mut history = Vec::with_capacity(block_count as usize);
        let from_block = upto_block.saturating_sub(block_count - 1);

        // Here we are requesting `fee_history` from blocks
        // `[from_block; upto_block]` in chunks of size `FEE_HISTORY_MAX_REQUEST_CHUNK`
        // starting from the oldest block.
        for chunk_start in (from_block..=upto_block).step_by(FEE_HISTORY_MAX_REQUEST_CHUNK) {
            let chunk_end = (chunk_start + FEE_HISTORY_MAX_REQUEST_CHUNK as u64).min(upto_block);
            let chunk_size = chunk_end - chunk_start + 1;

            let rewards: &[f64] = &[];
            let fee_history: L2FeeHistory = provider
                .raw_request(
                    "eth_feeHistory".into(),
                    (
                        U64::from(chunk_size),
                        BlockNumberOrTag::from(chunk_end),
                        rewards,
                    ),
                )
                .await
                .context("failed to get fee history from provider")?;

            if fee_history.base.oldest_block != chunk_start {
                anyhow::bail!(
                    "unexpected `oldest_block`, expected: {chunk_start}, got {}",
                    fee_history.base.oldest_block
                );
            }

            let pubdata_price_per_byte = fee_history
                .pubdata_price_per_byte
                .unwrap_or_default()
                .into_iter()
                .map(Some);
            // SYSCOIN: optional blob/gateway fields must not control whether we record L1 base-fee samples.
            history.extend(Self::fee_history_samples(
                fee_history.base.base_fee_per_gas,
                fee_history.base.base_fee_per_blob_gas,
                pubdata_price_per_byte,
                fixed_blob_base_fee,
                chunk_size as usize,
            ));
        }

        Ok(history)
    }

    // SYSCOIN: normalize optional fee-history extensions without dropping `baseFeePerGas`.
    fn fee_history_samples(
        base_fee_per_gas: impl IntoIterator<Item = u128>,
        base_fee_per_blob_gas: impl IntoIterator<Item = u128>,
        pubdata_price_per_byte: impl IntoIterator<Item = Option<U256>>,
        fixed_blob_base_fee: Option<u128>,
        chunk_size: usize,
    ) -> Vec<BaseFees> {
        let mut base_fee_per_blob_gas = base_fee_per_blob_gas.into_iter();
        let mut pubdata_price_per_byte = pubdata_price_per_byte.into_iter();

        // We take `chunk_size` entries and drop data for the block after `chunk_end`.
        base_fee_per_gas
            .into_iter()
            .take(chunk_size)
            .map(|base_fee_per_gas| {
                let base_fee_per_blob_gas = fixed_blob_base_fee
                    .unwrap_or_else(|| base_fee_per_blob_gas.next().unwrap_or_default());
                let pubdata_price_per_byte = pubdata_price_per_byte.next().unwrap_or_default();

                BaseFees {
                    base_fee_per_gas,
                    base_fee_per_blob_gas,
                    pubdata_price_per_byte,
                }
            })
            .collect()
    }

    // SYSCOIN
    async fn initial_base_fee_history(
        sl_provider: &DynProvider,
        config: &GasAdjusterConfig,
    ) -> anyhow::Result<(u64, Vec<BaseFees>)> {
        let fixed_blob_base_fee = if Self::uses_syscoin_blob_da(config.pubdata_mode) {
            Self::validate_bitcoin_da_fee_config(config)?;
            Some(Self::initial_syscoin_blob_base_fee(config).await?)
        } else {
            None
        };

        let current_block = sl_provider.get_block_number().await?.saturating_sub(1);
        let fee_history = Self::base_fee_history(
            sl_provider,
            current_block,
            config.max_base_fee_samples as u64,
            fixed_blob_base_fee,
        )
        .await?;
        Ok((current_block, fee_history))
    }

    // SYSCOIN
    async fn initial_syscoin_blob_base_fee(config: &GasAdjusterConfig) -> anyhow::Result<u128> {
        loop {
            match Self::bitcoin_blob_base_fee(config).await {
                Ok(fee) => return Ok(fee),
                Err(err) if Self::is_retriable_blob_fee_startup_error(&err) => {
                    // SYSCOIN: retry only the authoritative Syscoin DA fee fetch. Other
                    // initialization failures still surface immediately to operators.
                    tracing::warn!(
                        retry_after = ?config.poll_period,
                        error = %err,
                        "Failed to initialize blob-mode gas adjuster; retrying Syscoin fee fetch"
                    );
                    tokio::time::sleep(config.poll_period).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    // SYSCOIN
    fn uses_syscoin_blob_da(pubdata_mode: PubdataMode) -> bool {
        matches!(
            pubdata_mode,
            PubdataMode::Blobs | PubdataMode::RelayedL2Calldata
        )
    }

    // SYSCOIN
    fn validate_bitcoin_da_fee_config(config: &GasAdjusterConfig) -> anyhow::Result<()> {
        let rpc_url = config
            .bitcoin_da_rpc_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .context("missing bitcoin_da_rpc_url for blob fee estimation")?;
        let parsed_url =
            Url::parse(rpc_url).context("invalid bitcoin_da_rpc_url for blob fee estimation")?;
        anyhow::ensure!(
            matches!(parsed_url.scheme(), "http" | "https"),
            "invalid bitcoin_da_rpc_url scheme for blob fee estimation"
        );
        config
            .bitcoin_da_rpc_user
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .context("missing bitcoin_da_rpc_user for blob fee estimation")?;
        config
            .bitcoin_da_rpc_password
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .context("missing bitcoin_da_rpc_password for blob fee estimation")?;
        Ok(())
    }

    // SYSCOIN
    fn is_retriable_blob_fee_startup_error(err: &anyhow::Error) -> bool {
        let err = err.to_string();
        if err.contains("missing bitcoin_da_rpc_")
            || err.contains("invalid bitcoin_da_rpc_")
            || err.contains("RPC error:")
            || err.contains("failed to construct Syscoin client")
        {
            return false;
        }

        match Self::blob_fee_http_status(&err) {
            Some(408 | 429) => true,
            Some(status) => status >= 500,
            None => Self::is_transport_blob_fee_error(&err),
        }
    }

    // SYSCOIN
    fn blob_fee_http_status(err: &str) -> Option<u16> {
        let (_, after_marker) = err.split_once("HTTP error:")?;
        let status = after_marker
            .trim_start()
            .split(|ch: char| !ch.is_ascii_digit())
            .next()?;
        status.parse().ok()
    }

    // SYSCOIN
    fn is_transport_blob_fee_error(err: &str) -> bool {
        let err = err.to_ascii_lowercase();
        err.contains("error sending request")
            || err.contains("error trying to connect")
            || err.contains("dns error")
            || err.contains("tls")
            || err.contains("certificate")
            || err.contains("handshake")
            || err.contains("connection refused")
            || err.contains("connection reset")
            || err.contains("connection closed")
            || err.contains("deadline has elapsed")
            || err.contains("timed out")
            || err.contains("timeout")
    }

    // SYSCOIN
    async fn bitcoin_blob_base_fee(config: &GasAdjusterConfig) -> anyhow::Result<u128> {
        Self::validate_bitcoin_da_fee_config(config)?;

        let rpc_url = config
            .bitcoin_da_rpc_url
            .as_deref()
            .context("missing bitcoin_da_rpc_url for blob fee estimation")?;
        let rpc_user = config
            .bitcoin_da_rpc_user
            .as_deref()
            .context("missing bitcoin_da_rpc_user for blob fee estimation")?;
        let rpc_password = config
            .bitcoin_da_rpc_password
            .as_deref()
            .context("missing bitcoin_da_rpc_password for blob fee estimation")?;

        let client = SyscoinClient::new(
            rpc_url,
            rpc_user,
            rpc_password,
            &config.bitcoin_da_poda_url,
            Some(config.bitcoin_da_request_timeout),
            &config.bitcoin_da_wallet_name,
        )
        .map_err(|err| {
            anyhow::anyhow!("failed to construct Syscoin client for blob fee estimation: {err}")
        })?;

        client
            .get_blob_base_fee(config.bitcoin_da_fee_conf_target)
            .await
            .map_err(|err| anyhow::anyhow!("failed to estimate Syscoin blob base fee: {err}"))
    }
}

/// Information about the base fees provided by the L1 client.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BaseFees {
    pub base_fee_per_gas: u128,
    pub base_fee_per_blob_gas: u128,
    pub pubdata_price_per_byte: Option<U256>,
}

#[cfg(test)]
mod tests {
    use super::{BaseFees, GasAdjuster, GasAdjusterConfig};
    use alloy::primitives::U256;
    use std::time::Duration;
    use zksync_os_types::PubdataMode;

    fn gas_adjuster_config() -> GasAdjusterConfig {
        GasAdjusterConfig {
            pubdata_mode: PubdataMode::Blobs,
            max_base_fee_samples: 100,
            num_samples_for_blob_base_fee_estimate: 100,
            max_blob_fill_ratio_samples: 100,
            max_priority_fee_per_gas: 0,
            poll_period: Duration::from_secs(1),
            pubdata_pricing_multiplier: 1.0,
            bitcoin_da_rpc_url: Some("http://127.0.0.1:8370".to_owned()),
            bitcoin_da_rpc_user: Some("user".to_owned()),
            bitcoin_da_rpc_password: Some("password".to_owned()),
            bitcoin_da_poda_url: "http://127.0.0.1:8371".to_owned(),
            bitcoin_da_wallet_name: "zksync-os".to_owned(),
            bitcoin_da_request_timeout: Duration::from_secs(1),
            bitcoin_da_fee_conf_target: 6,
        }
    }

    #[test]
    fn fee_history_samples_keep_base_fees_when_blob_fees_are_missing() {
        let samples = GasAdjuster::fee_history_samples([100, 110, 120, 130], [], [], None, 3);

        assert_eq!(
            samples,
            vec![
                BaseFees {
                    base_fee_per_gas: 100,
                    base_fee_per_blob_gas: 0,
                    pubdata_price_per_byte: None,
                },
                BaseFees {
                    base_fee_per_gas: 110,
                    base_fee_per_blob_gas: 0,
                    pubdata_price_per_byte: None,
                },
                BaseFees {
                    base_fee_per_gas: 120,
                    base_fee_per_blob_gas: 0,
                    pubdata_price_per_byte: None,
                },
            ]
        );
    }

    #[test]
    fn fee_history_samples_apply_fixed_blob_fee_without_rpc_blob_fees() {
        let samples = GasAdjuster::fee_history_samples([100, 110, 120, 130], [], [], Some(7), 3);

        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.base_fee_per_blob_gas)
                .collect::<Vec<_>>(),
            vec![7, 7, 7]
        );
    }

    #[test]
    fn fee_history_samples_do_not_require_gateway_pubdata_prices() {
        let samples = GasAdjuster::fee_history_samples(
            [100, 110, 120, 130],
            [1, 2, 3, 4],
            [Some(U256::from(10u32))],
            None,
            3,
        );

        assert_eq!(
            samples,
            vec![
                BaseFees {
                    base_fee_per_gas: 100,
                    base_fee_per_blob_gas: 1,
                    pubdata_price_per_byte: Some(U256::from(10u32)),
                },
                BaseFees {
                    base_fee_per_gas: 110,
                    base_fee_per_blob_gas: 2,
                    pubdata_price_per_byte: None,
                },
                BaseFees {
                    base_fee_per_gas: 120,
                    base_fee_per_blob_gas: 3,
                    pubdata_price_per_byte: None,
                },
            ]
        );
    }

    #[test]
    fn bitcoin_da_fee_config_rejects_missing_or_empty_credentials() {
        let mut config = gas_adjuster_config();
        config.bitcoin_da_rpc_url = Some(" ".to_owned());

        let err = GasAdjuster::validate_bitcoin_da_fee_config(&config).unwrap_err();
        assert!(err.to_string().contains("missing bitcoin_da_rpc_url"));
    }

    #[test]
    fn bitcoin_da_fee_config_rejects_invalid_rpc_url() {
        let mut config = gas_adjuster_config();
        config.bitcoin_da_rpc_url = Some("not a url".to_owned());

        let err = GasAdjuster::validate_bitcoin_da_fee_config(&config).unwrap_err();
        assert!(err.to_string().contains("invalid bitcoin_da_rpc_url"));
    }

    #[test]
    fn syscoin_blob_da_modes_include_gateway_relayed_mode() {
        assert!(GasAdjuster::uses_syscoin_blob_da(PubdataMode::Blobs));
        assert!(GasAdjuster::uses_syscoin_blob_da(
            PubdataMode::RelayedL2Calldata
        ));
        assert!(!GasAdjuster::uses_syscoin_blob_da(PubdataMode::Calldata));
        assert!(!GasAdjuster::uses_syscoin_blob_da(PubdataMode::Validium));
    }

    #[test]
    fn blob_fee_startup_retry_filter_rejects_auth_errors() {
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 401 returned body: unauthorized"
            )
        ));
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 403 returned body: forbidden"
            )
        ));
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 404 returned body: not found"
            )
        ));
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to construct Syscoin client for blob fee estimation: invalid URL"
            )
        ));
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: RPC error: method not found"
            )
        ));
        assert!(!GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!("failed to estimate Syscoin blob base fee: malformed response")
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: error sending request for url"
            )
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!("failed to estimate Syscoin blob base fee: operation timed out")
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!("failed to estimate Syscoin blob base fee: dns error")
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!("failed to estimate Syscoin blob base fee: TLS handshake failed")
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 408 returned body: timeout"
            )
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 429 returned body: too many requests"
            )
        ));
        assert!(GasAdjuster::is_retriable_blob_fee_startup_error(
            &anyhow::anyhow!(
                "failed to estimate Syscoin blob base fee: HTTP error: 503 returned body: unavailable"
            )
        ));
    }
}
