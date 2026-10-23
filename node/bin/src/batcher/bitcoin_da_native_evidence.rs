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
    Confirmed,
    PresentAwaitingCanonicalEvidence,
    Unavailable,
}

pub(crate) struct NativeDaEvidenceProvider {
    provider: NodeProvider,
    chain_id: u64,
    timeout: Duration,
    observed_presence: Mutex<NativePresenceCache>,
    inclusion: Option<CanonicalInclusionResolver>,
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
            inclusion: None,
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
        let inclusion = CanonicalInclusionResolver::new(provider.clone(), config)?;
        let mut evidence = Self::new(provider, chain_id, config.bitcoin_da_request_timeout);
        evidence.inclusion = Some(inclusion);
        Ok(Some(evidence))
    }

    pub(super) async fn prepare_command(&self, hashes: &BTreeSet<String>) -> anyhow::Result<()> {
        if let Some(inclusion) = &self.inclusion {
            inclusion.prepare_command(hashes).await?;
        }
        Ok(())
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
        let evidence = {
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
                    NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
                        NativeDaEvidence::PresentAwaitingCanonicalEvidence
                    } else {
                        NativeDaEvidence::Unavailable
                    }
                }
            }
        };
        if evidence == NativeDaEvidence::PresentAwaitingCanonicalEvidence
            && let Some(inclusion) = &self.inclusion
        {
            match inclusion.resolve(hash).await {
                Ok(true) => return NativeDaEvidence::Confirmed,
                Ok(false) => {}
                Err(err) => tracing::warn!(
                    version_hash, %err,
                    "Bitcoin DA canonical inclusion unresolved; retaining fee suppression"
                ),
            }
        }
        evidence
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
pub(super) mod test_support {
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

    use super::{NativeDaEvidenceProvider, TANENBAUM_CHAIN_ID};
    use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
    use alloy::primitives::{B256, Bytes, U64};
    use axum::{Json, Router, routing::get};
    use serde_json::{Value, json};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum InclusionCase {
        Canonical,
        WrongCoreHash,
        WrongCoreCanonicalHash,
        Unconfirmed,
        StaleNeVmHash,
        WrongNeVmNumberFirst,
        WrongNeVmNumberLast,
    }

    pub(crate) struct InclusionFixture {
        pub(crate) config: BatcherConfig,
        pub(crate) provider: NodeProvider,
        pub(crate) asserter: Asserter,
        pub(crate) methods: Arc<Mutex<Vec<String>>>,
        pub(crate) archive_reads: Arc<AtomicUsize>,
        pub(crate) full_blob_reads: Arc<AtomicUsize>,
        pub(crate) server: tokio::task::JoinHandle<()>,
        hash: B256,
        case: InclusionCase,
    }

    impl InclusionFixture {
        pub(crate) fn evidence(&self) -> Arc<NativeDaEvidenceProvider> {
            Arc::new(
                NativeDaEvidenceProvider::for_config(
                    self.provider.clone(),
                    TANENBAUM_CHAIN_ID,
                    &self.config,
                )
                .unwrap()
                .unwrap(),
            )
        }

        pub(crate) fn assert_only_read_effects(&self) {
            assert_eq!(self.archive_reads.load(Ordering::SeqCst), 0);
            assert_eq!(self.full_blob_reads.load(Ordering::SeqCst), 0);
            for method in self.methods.lock().unwrap().iter() {
                assert!(
                    matches!(
                        method.as_str(),
                        "getnevmblobdata"
                            | "getblockchaininfo"
                            | "getblock"
                            | "getblockhash"
                            | "getblockheader"
                    ),
                    "unexpected wallet or other RPC: {method}"
                );
            }
        }

        pub(crate) fn queue_lookup(&self, first: bool) {
            let head = json!({"number":"0x1","hash":B256::repeat_byte(0x44)});
            self.asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
            self.asserter
                .push_success(&Bytes::copy_from_slice(self.hash.as_slice()));
            self.asserter.push_success(&U64::from(TANENBAUM_CHAIN_ID));
            self.asserter.push_success(&head);
            if first {
                self.asserter.push_success(&json!({"chainId":"0x1644","headNumber":"0x1","headHash":B256::repeat_byte(0x44),
                    "searchedFrom":"0x1","searchedTo":"0x1","exhausted":true,
                    "inclusions":[{"dataHash":self.hash,"nevmBlockNumber":"0x1","nevmBlockHash":B256::repeat_byte(0x44),"syscoinBlockHash":B256::repeat_byte(0x11)}]}));
                self.asserter.push_success(&head);
            }
            let mut candidate = head.clone();
            if self.case == InclusionCase::WrongNeVmNumberFirst {
                candidate["number"] = json!("0x2");
            }
            if self.case == InclusionCase::StaleNeVmHash {
                candidate["hash"] = json!(B256::repeat_byte(0x55));
            }
            self.asserter.push_success(&candidate);
            if matches!(
                self.case,
                InclusionCase::Canonical | InclusionCase::WrongNeVmNumberLast
            ) {
                self.asserter
                    .push_success(&Bytes::copy_from_slice(self.hash.as_slice()));
                let mut final_header = head.clone();
                if self.case == InclusionCase::WrongNeVmNumberLast {
                    final_header["number"] = json!("0x2");
                }
                self.asserter.push_success(&final_header);
            }
            if self.case != InclusionCase::Canonical {
                self.asserter.push_success(&json!({"chainId":"0x1644","headNumber":"0x1","headHash":B256::repeat_byte(0x44),"inclusions":[],"exhausted":true}));
                self.asserter.push_success(&head);
            }
        }
    }

    pub(crate) async fn inclusion_fixture(hash: B256, case: InclusionCase) -> InclusionFixture {
        let methods = Arc::new(Mutex::new(Vec::new()));
        let archive_reads = Arc::new(AtomicUsize::new(0));
        let full_blob_reads = Arc::new(AtomicUsize::new(0));
        let get_reads = archive_reads.clone();
        let rpc_methods = methods.clone();
        let rpc_full_reads = full_blob_reads.clone();
        let app = Router::new().fallback(get(move || {
            let reads = get_reads.clone();
            async move { reads.fetch_add(1,Ordering::SeqCst); "unexpected archive read" }
        }).post(move |Json(request):Json<Value>| {
            let methods = rpc_methods.clone();
            let full_reads = rpc_full_reads.clone();
            async move {
                let method = request["method"].as_str().unwrap_or("malformed");
                methods.lock().unwrap().push(method.to_owned());
                let sys_hash = if case == InclusionCase::WrongCoreHash { B256::repeat_byte(0x66) } else { B256::repeat_byte(0x11) };
                let script = [vec![0x6a,34,33,1],hash.as_slice().to_vec()].concat();
                let result = match method {
                    "getnevmblobdata" => {
                        if request["params"][1] == true { full_reads.fetch_add(1, Ordering::SeqCst); }
                        return Json(json!({"id":request["id"],"error":{"code":-32602,"message":"missing metadata"}}));
                    }
                    "getblockchaininfo" => json!({"chain":"test"}),
                    "getblock" => json!({"hash":alloy::hex::encode(sys_hash),"height":123,"confirmations":if case==InclusionCase::Unconfirmed {0} else {1},
                        "tx":[{"txid":"33".repeat(32),"version":137,"vout":[{"n":0,"scriptPubKey":{"hex":alloy::hex::encode(script)}}]}]}),
                    "getblockheader" => json!({"hash":alloy::hex::encode(B256::repeat_byte(0x11)),"height":123,"confirmations":1}),
                    "getblockhash" => json!(alloy::hex::encode(if case==InclusionCase::WrongCoreCanonicalHash {B256::repeat_byte(0x77)} else {B256::repeat_byte(0x11)})),
                    _ => return Json(json!({"id":request["id"],"error":{"code":-32601,"message":"unexpected method"}})),
                };
                Json(json!({"id":request["id"],"result":result,"error":null}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        InclusionFixture {
            config: BatcherConfig {
                bitcoin_da_rpc_url: Some(url.clone()),
                bitcoin_da_rpc_user: Some("test-user".into()),
                bitcoin_da_rpc_password: Some("test-password".into()),
                bitcoin_da_poda_url: url,
                bitcoin_da_finality_mode: BitcoinDaFinalityMode::Confirmations,
                bitcoin_da_finality_confirmations: 1,
                bitcoin_da_request_timeout: Duration::from_secs(1),
                bitcoin_da_finality_timeout: Duration::from_millis(1),
                bitcoin_da_missing_blob_recovery_timeout: Some(Duration::from_millis(1)),
                bitcoin_da_finality_poll_interval: Duration::from_millis(1),
                bitcoin_da_gateway_l1_republish_enabled: true,
                ..Default::default()
            },
            provider,
            asserter,
            methods,
            archive_reads,
            full_blob_reads,
            server,
            hash,
            case,
        }
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
    async fn configured_provider_confirms_original_canonical_core_publication() {
        use super::test_support::{InclusionCase, inclusion_fixture};
        let fixture = inclusion_fixture(HASH, InclusionCase::Canonical).await;
        let evidence = fixture.evidence();
        evidence
            .prepare_command(&BTreeSet::from([HASH.to_string()]))
            .await
            .unwrap();
        fixture.queue_lookup(true);
        assert_eq!(
            evidence.resolve_missing(&HASH.to_string()).await,
            NativeDaEvidence::Confirmed
        );
        assert!(fixture.asserter.read_q().is_empty());
        fixture.assert_only_read_effects();
        assert!(
            fixture
                .methods
                .lock()
                .unwrap()
                .iter()
                .any(|method| method == "getblock")
        );
        fixture.server.abort();
    }

    #[tokio::test]
    async fn configured_provider_rejects_stale_unconfirmed_and_wrong_number_locators() {
        use super::test_support::{InclusionCase, inclusion_fixture};
        for case in [
            InclusionCase::WrongCoreHash,
            InclusionCase::WrongCoreCanonicalHash,
            InclusionCase::Unconfirmed,
            InclusionCase::StaleNeVmHash,
            InclusionCase::WrongNeVmNumberFirst,
            InclusionCase::WrongNeVmNumberLast,
        ] {
            let fixture = inclusion_fixture(HASH, case).await;
            let evidence = fixture.evidence();
            evidence
                .prepare_command(&BTreeSet::from([HASH.to_string()]))
                .await
                .unwrap();
            fixture.queue_lookup(true);
            assert_eq!(
                evidence.resolve_missing(&HASH.to_string()).await,
                NativeDaEvidence::PresentAwaitingCanonicalEvidence,
                "case {case:?}"
            );
            assert!(fixture.asserter.read_q().is_empty(), "case {case:?}");
            fixture.assert_only_read_effects();
            fixture.server.abort();
        }
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
            NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
                NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
            NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
                NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
            NativeDaEvidence::PresentAwaitingCanonicalEvidence
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
use super::bitcoin_da_inclusion::CanonicalInclusionResolver;
use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
