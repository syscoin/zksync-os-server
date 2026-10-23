use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
use alloy::eips::BlockId;
use alloy::hex;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::Context;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;
use zksync_os_provider::NodeProvider;

pub(super) const TANENBAUM_CHAIN_ID: u64 = 5_700;
const DATA_HASH_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x63,
]);
const MAX_REMEMBERED_NATIVE_HASHES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeDaEvidence {
    Absent,
    Registered,
    PreviouslyRegistered,
    Unavailable,
}

pub(crate) struct NativeDaEvidenceProvider {
    provider: NodeProvider,
    chain_id: u64,
    timeout: Duration,
    observed_presence: Mutex<NativePresenceCache>,
}

#[derive(Default)]
struct NativePresenceCache {
    hashes: BTreeSet<B256>,
    overflowed: bool,
}

impl NativeDaEvidenceProvider {
    pub(super) fn new(provider: NodeProvider, chain_id: u64, timeout: Duration) -> Self {
        Self {
            provider,
            chain_id,
            timeout,
            observed_presence: Mutex::new(NativePresenceCache::default()),
        }
    }

    pub(crate) fn for_config(
        provider: NodeProvider,
        chain_id: u64,
        config: &BatcherConfig,
    ) -> anyhow::Result<Option<Self>> {
        if chain_id != TANENBAUM_CHAIN_ID
            || config.bitcoin_da_finality_mode != BitcoinDaFinalityMode::Confirmations
            || config.bitcoin_da_finality_confirmations != 1
        {
            return Ok(None);
        }
        Ok(Some(Self::new(
            provider,
            chain_id,
            config.bitcoin_da_request_timeout,
        )))
    }

    pub(super) fn forget(&self, version_hash: &str) {
        if let Ok(hash) = parse_version_hash(version_hash) {
            self.observed_presence.lock().unwrap().hashes.remove(&hash);
        }
    }

    pub(super) fn clear(&self) {
        *self.observed_presence.lock().unwrap() = NativePresenceCache::default();
    }

    pub(super) async fn resolve_missing(&self, version_hash: &str) -> NativeDaEvidence {
        if self.chain_id != TANENBAUM_CHAIN_ID {
            return NativeDaEvidence::Absent;
        }
        let hash = match parse_version_hash(version_hash) {
            Ok(hash) => hash,
            Err(err) => {
                tracing::warn!(version_hash, %err, "invalid Bitcoin DA native presence request");
                return NativeDaEvidence::Unavailable;
            }
        };
        let lookup = async {
            anyhow::ensure!(
                self.provider.get_chain_id().await? == self.chain_id,
                "Bitcoin DA NEVM provider chain ID changed"
            );
            let request = TransactionRequest::default()
                .with_to(DATA_HASH_PRECOMPILE)
                .with_input(Bytes::copy_from_slice(hash.as_slice()));
            let output = self.provider.call(request).block(BlockId::latest()).await?;
            if output.is_empty() {
                return Ok(false);
            }
            anyhow::ensure!(
                output.as_ref() == hash.as_slice(),
                "Bitcoin DA NEVM precompile returned a non-matching version hash"
            );
            Ok::<_, anyhow::Error>(true)
        };
        let result = match tokio::time::timeout(self.timeout, lookup).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("Bitcoin DA NEVM presence check timed out")),
        };
        {
            let mut observed = self.observed_presence.lock().unwrap();
            match result {
                Ok(true) => {
                    if observed.hashes.len() < MAX_REMEMBERED_NATIVE_HASHES {
                        observed.hashes.insert(hash);
                    } else if !observed.hashes.contains(&hash) {
                        // Bound memory without forgetting a positive and spending on a later
                        // ambiguous read. The command must finish before this fence is cleared.
                        observed.overflowed = true;
                        tracing::warn!(
                            "Bitcoin DA native presence cache is full; retaining fee suppression until this command completes"
                        );
                    }
                    NativeDaEvidence::Registered
                }
                Ok(false) => {
                    observed.hashes.remove(&hash);
                    NativeDaEvidence::Absent
                }
                Err(err) => {
                    // An unavailable read cannot turn previously observed native presence into
                    // evidence that another paid publication is needed.
                    let known_present = observed.hashes.contains(&hash) || observed.overflowed;
                    tracing::warn!(
                        version_hash,
                        %err,
                        known_present,
                        "Bitcoin DA NEVM presence check unavailable"
                    );
                    if known_present {
                        NativeDaEvidence::PreviouslyRegistered
                    } else {
                        NativeDaEvidence::Unavailable
                    }
                }
            }
        }
    }
}

fn parse_version_hash(version_hash: &str) -> anyhow::Result<B256> {
    let hash = hex::decode(version_hash.strip_prefix("0x").unwrap_or(version_hash))
        .context("invalid Bitcoin DA version hash")?;
    anyhow::ensure!(
        hash.len() == 32,
        "Bitcoin DA version hash must have 32 bytes"
    );
    Ok(B256::from_slice(&hash))
}

#[cfg(test)]
pub(crate) mod test_support {
    use alloy::network::EthereumWallet;
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::rpc::types::Header;
    use alloy::transports::mock::Asserter;
    use zksync_os_provider::NodeProvider;

    pub(crate) async fn mocked_provider(asserter: &Asserter) -> NodeProvider {
        let header: Header = Default::default();
        asserter.push_success(&header);
        asserter.push_success(&header);
        asserter.push_failure(ErrorPayload::method_not_found());
        asserter.push_success(&"anvil/v1.0.0");
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        NodeProvider::new(provider).await.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::mocked_provider;
    use super::*;
    use alloy::primitives::U64;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::mock::Asserter;

    const HASH: B256 = B256::repeat_byte(0x31);

    #[tokio::test]
    async fn configured_provider_uses_registration_without_core_configuration() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let config = BatcherConfig {
            bitcoin_da_finality_mode: BitcoinDaFinalityMode::Confirmations,
            bitcoin_da_finality_confirmations: 1,
            ..Default::default()
        };
        let evidence = NativeDaEvidenceProvider::for_config(provider, TANENBAUM_CHAIN_ID, &config)
            .unwrap()
            .unwrap();
        asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
        asserter.push_success(&Bytes::copy_from_slice(HASH.as_slice()));
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Registered
        );
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn native_absence_requires_a_successful_empty_response() {
        for (output, expected) in [
            (Bytes::new(), NativeDaEvidence::Absent),
            (Bytes::from(vec![0x31; 31]), NativeDaEvidence::Unavailable),
            (Bytes::from(vec![0x31; 33]), NativeDaEvidence::Unavailable),
            (Bytes::from(vec![0x32; 32]), NativeDaEvidence::Unavailable),
        ] {
            let asserter = Asserter::new();
            let provider = mocked_provider(&asserter).await;
            asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
            asserter.push_success(&output);
            let evidence =
                NativeDaEvidenceProvider::new(provider, TANENBAUM_CHAIN_ID, Duration::from_secs(1));
            assert_eq!(evidence.resolve_missing(&HASH.to_string()).await, expected);
            assert!(asserter.read_q().is_empty());
        }
    }

    #[tokio::test]
    async fn unavailable_native_lookup_preserves_prior_presence_until_a_definite_absence() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let evidence =
            NativeDaEvidenceProvider::new(provider, TANENBAUM_CHAIN_ID, Duration::from_secs(1));
        asserter.push_failure(ErrorPayload::method_not_found());
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Unavailable
        );

        asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
        asserter.push_success(&Bytes::copy_from_slice(HASH.as_slice()));
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Registered
        );
        for malformed in [false, true] {
            asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
            if malformed {
                asserter.push_success(&Bytes::from(vec![0xff; 32]));
            } else {
                asserter.push_failure(ErrorPayload::method_not_found());
            }
            assert_eq!(
                evidence.resolve_missing(&HASH.to_string()).await,
                NativeDaEvidence::PreviouslyRegistered
            );
        }

        asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
        asserter.push_success(&Bytes::new());
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Absent
        );
        asserter.push_failure(ErrorPayload::method_not_found());
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Unavailable
        );
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn native_lookup_requires_tanenbaum_and_the_authenticated_provider_identity() {
        for (expected, observed, evidence_result) in [
            (57, None, NativeDaEvidence::Absent),
            (TANENBAUM_CHAIN_ID, Some(57), NativeDaEvidence::Unavailable),
        ] {
            let asserter = Asserter::new();
            let provider = mocked_provider(&asserter).await;
            if let Some(observed) = observed {
                asserter.push_success(&U64::from(observed));
            }
            let evidence =
                NativeDaEvidenceProvider::new(provider, expected, Duration::from_secs(1));
            assert_eq!(
                evidence.resolve_missing(&HASH.to_string()).await,
                evidence_result
            );
            assert!(asserter.read_q().is_empty());
        }
    }

    #[tokio::test]
    async fn malformed_input_never_reaches_the_provider_and_forget_clears_presence() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let evidence =
            NativeDaEvidenceProvider::new(provider, TANENBAUM_CHAIN_ID, Duration::from_secs(1));
        for hash in ["00", "0xzz", "0x", "0X3131"] {
            assert_eq!(
                evidence.resolve_missing(hash).await,
                NativeDaEvidence::Unavailable
            );
        }
        asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
        asserter.push_success(&Bytes::copy_from_slice(HASH.as_slice()));
        let uppercase = format!("0x{}", hex::encode(HASH).to_uppercase());
        assert_eq!(
            evidence.resolve_missing(&uppercase).await,
            NativeDaEvidence::Registered
        );
        evidence.forget(&HASH.to_string());
        asserter.push_failure(ErrorPayload::method_not_found());
        assert_eq!(
            evidence.resolve_missing(&uppercase).await,
            NativeDaEvidence::Unavailable
        );
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn native_presence_cache_is_bounded_and_overflow_never_enables_paid_recovery() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let evidence =
            NativeDaEvidenceProvider::new(provider, TANENBAUM_CHAIN_ID, Duration::from_secs(1));
        for number in 0..=MAX_REMEMBERED_NATIVE_HASHES {
            let hash = B256::from(alloy::primitives::U256::from(number).to_be_bytes::<32>());
            asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
            asserter.push_success(&Bytes::copy_from_slice(hash.as_slice()));
            assert_eq!(
                evidence.resolve_missing(&hash.to_string()).await,
                NativeDaEvidence::Registered
            );
        }
        {
            let observed = evidence.observed_presence.lock().unwrap();
            assert_eq!(observed.hashes.len(), MAX_REMEMBERED_NATIVE_HASHES);
            assert!(observed.overflowed);
        }
        let overflow_hash = B256::from(
            alloy::primitives::U256::from(MAX_REMEMBERED_NATIVE_HASHES).to_be_bytes::<32>(),
        );
        asserter.push_failure(ErrorPayload::method_not_found());
        assert_eq!(
            evidence.resolve_missing(&overflow_hash.to_string()).await,
            NativeDaEvidence::PreviouslyRegistered
        );
        evidence.clear();
        asserter.push_failure(ErrorPayload::method_not_found());
        assert_eq!(
            evidence.resolve_missing(&overflow_hash.to_string()).await,
            NativeDaEvidence::Unavailable
        );
        assert!(asserter.read_q().is_empty());
    }
}
