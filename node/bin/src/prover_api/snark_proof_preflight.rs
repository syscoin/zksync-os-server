use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256};
use alloy::providers::Provider;
use async_trait::async_trait;
use std::time::Duration;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope, SnarkProof};
use zksync_os_contract_interface::ZkChain;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_provider::NodeProvider;

// SYSCOIN: One cumulative wall-clock budget covers every identity, topology, verifier, and
// canonical-tip RPC read. Provider-level retries must never hold an authenticated upload forever.
const DEFAULT_SNARK_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);

/// SYSCOIN: Only a canonical on-chain false result or data-bearing EVM revert is terminal for the
/// exact prover lease. Every topology, reorg, timeout, and RPC ambiguity remains retryable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnarkProofPreflightError {
    #[error("settlement verifier preflight is temporarily unavailable")]
    Unavailable,
    #[error("settlement verifier rejected the SNARK proof")]
    Rejected,
}

/// SYSCOIN: Abstract the admission decision so manager tests can prove exact lease disposition
/// without standing up an RPC server or weakening the production on-chain implementation.
#[async_trait]
pub trait SnarkProofPreflight: Send + Sync {
    async fn verify(
        &self,
        batches: &[SignedBatchEnvelope<FriProof>],
        proof: &SnarkProof,
    ) -> Result<(), SnarkProofPreflightError>;
}

/// SYSCOIN: Binds every real wrapper upload to the active production verifier selected by the
/// startup-discovered settlement-layer diamond and VK.
pub struct OnchainSnarkProofPreflight {
    provider: NodeProvider,
    diamond: ZkChain<NodeProvider>,
    expected_sl_chain_id: u64,
    expected_vk_hash: B256,
    timeout: Duration,
}

impl OnchainSnarkProofPreflight {
    pub fn new(
        provider: NodeProvider,
        diamond_address: Address,
        expected_sl_chain_id: u64,
        expected_vk_hash: B256,
    ) -> Self {
        Self {
            diamond: ZkChain::new(diamond_address, provider.clone()),
            provider,
            expected_sl_chain_id,
            expected_vk_hash,
            timeout: DEFAULT_SNARK_PREFLIGHT_TIMEOUT,
        }
    }

    async fn verify_at_canonical_tip(
        &self,
        batches: &[SignedBatchEnvelope<FriProof>],
        proof: &SnarkProof,
    ) -> Result<(), SnarkProofPreflightError> {
        // SYSCOIN: Build the exact Executor/wrapper arguments before RPC. An internal metadata
        // invariant failure is not evidence that the prover's cryptographic proof is invalid.
        let input = ProofCommand::zksync_os_verifier_input(batches, proof)
            .map_err(|_| SnarkProofPreflightError::Unavailable)?;

        let chain_id_before = self
            .provider
            .get_chain_id()
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?;
        if chain_id_before != self.expected_sl_chain_id {
            return Err(SnarkProofPreflightError::Unavailable);
        }

        // SYSCOIN: Anchor all dependent contract calls to one canonical EIP-1898 block hash.
        let anchor = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?
            .ok_or(SnarkProofPreflightError::Unavailable)?;
        let anchor_number = anchor.header.inner.number;
        let anchor_hash = anchor.header.hash;
        let block_id = BlockId::hash_canonical(anchor_hash);

        let verifier = self
            .diamond
            .get_verifier(block_id)
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?;
        if verifier == Address::ZERO {
            return Err(SnarkProofPreflightError::Unavailable);
        }
        let (is_testnet, vk_hash) = self
            .diamond
            .get_zksync_os_verifier_mode(verifier, block_id)
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?;
        // SYSCOIN: A zero compiled key is the deliberate regeneration sentinel. Never let it
        // become a usable production verifier identity even if a deployment is misconfigured.
        if is_testnet || self.expected_vk_hash == B256::ZERO || vk_hash != self.expected_vk_hash {
            return Err(SnarkProofPreflightError::Unavailable);
        }

        let verify_result = self
            .diamond
            .verify_zksync_os_proof_at_block(verifier, input.public_inputs, input.proof, block_id)
            .await;

        // SYSCOIN: Re-read the numbered anchor after the potentially expensive verifier call.
        // A reorg makes even a false/revert ambiguous, so retain the lease and retry from scratch.
        let canonical = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Number(anchor_number))
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?
            .ok_or(SnarkProofPreflightError::Unavailable)?;
        if canonical.header.inner.number != anchor_number || canonical.header.hash != anchor_hash {
            return Err(SnarkProofPreflightError::Unavailable);
        }
        let chain_id_after = self
            .provider
            .get_chain_id()
            .await
            .map_err(|_| SnarkProofPreflightError::Unavailable)?;
        if chain_id_after != self.expected_sl_chain_id {
            return Err(SnarkProofPreflightError::Unavailable);
        }

        match verify_result {
            Ok(true) => Ok(()),
            Ok(false) => Err(SnarkProofPreflightError::Rejected),
            // SYSCOIN: Data-bearing EVM revert at the exact validated contract/block is a
            // definitive rejection. Empty/absent revert data can also represent provider or gas
            // ambiguity, so it remains retryable and never revokes a potentially valid lease.
            Err(error)
                if error
                    .as_revert_data()
                    .is_some_and(|revert_data| !revert_data.is_empty()) =>
            {
                Err(SnarkProofPreflightError::Rejected)
            }
            Err(_) => Err(SnarkProofPreflightError::Unavailable),
        }
    }
}

#[async_trait]
impl SnarkProofPreflight for OnchainSnarkProofPreflight {
    async fn verify(
        &self,
        batches: &[SignedBatchEnvelope<FriProof>],
        proof: &SnarkProof,
    ) -> Result<(), SnarkProofPreflightError> {
        match tokio::time::timeout(self.timeout, self.verify_at_canonical_tip(batches, proof)).await
        {
            Ok(result) => result,
            Err(_) => Err(SnarkProofPreflightError::Unavailable),
        }
    }
}

/// SYSCOIN: Focused queue/journal unit tests do not deploy an EVM verifier. This implementation is
/// cfg-gated, so no production constructor can acknowledge a wrapper without on-chain preflight.
#[cfg(test)]
pub struct AcceptingTestSnarkProofPreflight;

#[cfg(test)]
#[async_trait]
impl SnarkProofPreflight for AcceptingTestSnarkProofPreflight {
    async fn verify(
        &self,
        _batches: &[SignedBatchEnvelope<FriProof>],
        _proof: &SnarkProof,
    ) -> Result<(), SnarkProofPreflightError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::test_util::create_test_batch_envelope_with_data;
    use alloy::network::EthereumWallet;
    use alloy::primitives::{Bytes, U64};
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::rpc::types::{Block, Header};
    use alloy::sol_types::SolValue;
    use alloy::transports::mock::Asserter;
    use serde_json::value::RawValue;
    use std::borrow::Cow;
    use zksync_os_batch_types::batcher_model::{RealSnarkProof, SnarkProof};
    use zksync_os_l1_sender::commands::prove::ZKSYNC_OS_V8_REAL_PROOF_BYTES;
    use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

    const SL_CHAIN_ID: u64 = 270;
    const DIAMOND: Address = Address::new([0x11; 20]);
    const VERIFIER: Address = Address::new([0x22; 20]);
    const VK_HASH: B256 = B256::new([0x33; 32]);

    fn header_with_number(number: u64) -> Header {
        let mut block: Block = Block::default();
        block.header.inner.number = number;
        block.header
    }

    fn block(number: u64, hash: B256) -> Block {
        let mut block: Block = Block::default();
        block.header.inner.number = number;
        block.header.hash = hash;
        block
    }

    async fn mocked_provider(asserter: &Asserter) -> NodeProvider {
        // SYSCOIN: NodeProvider capability discovery precedes the preflight call sequence.
        asserter.push_success(&header_with_number(1));
        asserter.push_success(&header_with_number(1));
        asserter.push_failure(ErrorPayload::method_not_found());
        asserter.push_success(&"anvil/v1.0.0".to_owned());
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone());
        NodeProvider::new(provider)
            .await
            .expect("mocked provider construction")
    }

    fn proof_inputs() -> (Vec<SignedBatchEnvelope<FriProof>>, SnarkProof) {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut batches = Vec::new();
        let mut previous = None;
        for batch_number in 1..=2 {
            let mut batch = create_test_batch_envelope_with_data(
                batch_number,
                protocol_version.clone(),
                FriProof::AlreadySubmittedToL1,
            );
            if let Some(previous) = previous {
                batch.batch.previous_stored_batch_info = previous;
            }
            previous = Some(batch.batch.batch_info.clone().into_stored());
            batches.push(batch);
        }
        (
            batches,
            SnarkProof::Real(RealSnarkProof {
                proof: vec![0x44; ZKSYNC_OS_V8_REAL_PROOF_BYTES],
                proving_execution_version: ProvingVersion::V8 as u32,
            }),
        )
    }

    fn push_identity_and_topology(
        asserter: &Asserter,
        anchor: &Block,
        is_testnet: bool,
        vk_hash: B256,
    ) {
        asserter.push_success(&U64::from(SL_CHAIN_ID));
        asserter.push_success(anchor);
        asserter.push_success(&Bytes::from(VERIFIER.abi_encode()));
        asserter.push_success(&Bytes::from(is_testnet.abi_encode()));
        asserter.push_success(&Bytes::from(vk_hash.abi_encode()));
    }

    fn push_postcheck(asserter: &Asserter, canonical: &Block) {
        asserter.push_success(canonical);
        asserter.push_success(&U64::from(SL_CHAIN_ID));
    }

    #[tokio::test]
    async fn accepts_true_at_one_canonical_production_snapshot() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let anchor = block(42, B256::new([0x42; 32]));
        push_identity_and_topology(&asserter, &anchor, false, VK_HASH);
        asserter.push_success(&Bytes::from(true.abi_encode()));
        push_postcheck(&asserter, &anchor);
        let verifier = OnchainSnarkProofPreflight::new(provider, DIAMOND, SL_CHAIN_ID, VK_HASH);
        let (batches, proof) = proof_inputs();

        let result = verifier.verify(&batches, &proof).await;
        assert_eq!(
            result,
            Ok(()),
            "unexpected preflight result; remaining mock responses: {:?}",
            &*asserter.read_q()
        );
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    #[tokio::test]
    async fn canonical_false_is_definitive_rejection() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let anchor = block(42, B256::new([0x42; 32]));
        push_identity_and_topology(&asserter, &anchor, false, VK_HASH);
        asserter.push_success(&Bytes::from(false.abi_encode()));
        push_postcheck(&asserter, &anchor);
        let verifier = OnchainSnarkProofPreflight::new(provider, DIAMOND, SL_CHAIN_ID, VK_HASH);
        let (batches, proof) = proof_inputs();

        assert_eq!(
            verifier.verify(&batches, &proof).await,
            Err(SnarkProofPreflightError::Rejected)
        );
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    #[tokio::test]
    async fn data_bearing_revert_is_definitive_only_after_canonical_postcheck() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let anchor = block(42, B256::new([0x42; 32]));
        push_identity_and_topology(&asserter, &anchor, false, VK_HASH);
        asserter.push_failure(ErrorPayload {
            code: 3,
            message: Cow::Borrowed("execution reverted"),
            data: Some(
                RawValue::from_string("\"0xdeadbeef\"".to_owned()).expect("valid raw revert data"),
            ),
        });
        push_postcheck(&asserter, &anchor);
        let verifier = OnchainSnarkProofPreflight::new(provider, DIAMOND, SL_CHAIN_ID, VK_HASH);
        let (batches, proof) = proof_inputs();

        assert_eq!(
            verifier.verify(&batches, &proof).await,
            Err(SnarkProofPreflightError::Rejected)
        );
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    #[tokio::test]
    async fn reorg_overrides_a_definitive_verifier_result() {
        let asserter = Asserter::new();
        let provider = mocked_provider(&asserter).await;
        let anchor = block(42, B256::new([0x42; 32]));
        let replacement = block(42, B256::new([0x43; 32]));
        push_identity_and_topology(&asserter, &anchor, false, VK_HASH);
        asserter.push_success(&Bytes::from(false.abi_encode()));
        asserter.push_success(&replacement);
        let verifier = OnchainSnarkProofPreflight::new(provider, DIAMOND, SL_CHAIN_ID, VK_HASH);
        let (batches, proof) = proof_inputs();

        assert_eq!(
            verifier.verify(&batches, &proof).await,
            Err(SnarkProofPreflightError::Unavailable)
        );
        assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
    }

    #[tokio::test]
    async fn testnet_or_wrong_vk_topology_is_retryable_not_proof_rejection() {
        for (is_testnet, vk_hash) in [(true, VK_HASH), (false, B256::new([0x34; 32]))] {
            let asserter = Asserter::new();
            let provider = mocked_provider(&asserter).await;
            let anchor = block(42, B256::new([0x42; 32]));
            push_identity_and_topology(&asserter, &anchor, is_testnet, vk_hash);
            let verifier = OnchainSnarkProofPreflight::new(provider, DIAMOND, SL_CHAIN_ID, VK_HASH);
            let (batches, proof) = proof_inputs();

            assert_eq!(
                verifier.verify(&batches, &proof).await,
                Err(SnarkProofPreflightError::Unavailable)
            );
            assert!(asserter.read_q().is_empty(), "all RPC responses consumed");
        }
    }
}
