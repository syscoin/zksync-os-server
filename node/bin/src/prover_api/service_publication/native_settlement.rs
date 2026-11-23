//! A service acceptance and an advance through the native proof path have different
//! accounting. Only the latter can retire a superseded wrapper without a Sentry receipt.
use super::{IZkSysProofGateV1, ServicePublication, SignedBatchEnvelope, StoredBatch, Work};
use alloy::{
    primitives::{Address, B256, U256, keccak256},
    providers::Provider,
    rpc::types::{BlockId, BlockNumberOrTag, Filter},
    sol_types::{SolEvent, SolValue},
};
use anyhow::Context;
use zksync_os_batch_types::batcher_model::FriProof;
use zksync_os_contract_interface::{IExecutor::StoredBatchInfo, ZkChain};
use zksync_os_l1_sender::config::ConfirmationPolicy;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NativeObservation {
    Pending,
    NoGap,
    Confirmed(B256),
}

pub(super) struct NativeBatch {
    chain: Address,
    child_chain_id: U256,
    settlement_chain_id: U256,
    required_confirmations: u64,
    number: u64,
    hash: B256,
}

impl NativeBatch {
    pub(super) fn from_work(work: &Work, number: u64) -> anyhow::Result<Self> {
        let (_, batches, _) =
            <(StoredBatch, Vec<StoredBatch>, Vec<U256>)>::abi_decode_params_validate(
                work.proof_data
                    .get(1..)
                    .context("missing native proof data")?,
            )?;
        let index = usize::try_from(
            number
                .checked_sub(work.batch_from)
                .context("batch precedes service work")?,
        )?;
        let stored = batches
            .get(index)
            .context("batch missing from service work")?;
        anyhow::ensure!(
            number <= work.batch_to && stored.batchNumber == number,
            "native batch differs from service work"
        );
        Ok(Self {
            chain: work.chain_address,
            child_chain_id: work.execution_chain_id,
            settlement_chain_id: work.settlement_chain_id,
            required_confirmations: work.required_confirmations,
            number,
            hash: keccak256(stored.abi_encode()),
        })
    }

    pub(super) fn from_batch(
        batch: &SignedBatchEnvelope<FriProof>,
        required_confirmations: u64,
    ) -> Self {
        let info = &batch.batch.batch_info.commit_info;
        let stored = StoredBatchInfo::from(&batch.batch.batch_info.clone().into_stored());
        Self {
            chain: batch.batch.chain_address,
            child_chain_id: U256::from(info.chain_id),
            settlement_chain_id: U256::from(info.sl_chain_id),
            required_confirmations,
            number: batch.batch_number(),
            hash: keccak256(stored.abi_encode()),
        }
    }

    /// Locate the first observation of this native batch. Checking an event at that
    /// height distinguishes service acceptance from emergency progress even when both happen
    /// within one settlement block. No local marker or mutable latest counter grants credit.
    pub(super) async fn confirmed_gap(
        &self,
        publication: &ServicePublication,
    ) -> anyhow::Result<NativeObservation> {
        let provider = &publication.provider;
        let config = &publication.config;
        let gate = IZkSysProofGateV1::new(config.gate, provider.clone());
        // While native work is still pending, one cheap latest read avoids a historical
        // lookup on every publication poll. All positive decisions use pinned state below.
        if gate.observedNativeBatch().call().await? < self.number {
            return Ok(NativeObservation::Pending);
        }
        let chain = ZkChain::new(self.chain, provider.clone());
        let tip = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("missing settlement tip")?;
        let policy = ConfirmationPolicy::new(
            self.required_confirmations
                .max(publication.confirmation_policy.required_confirmations()),
        );
        let height = policy.safe_state_block(tip.header.inner.number);
        let safe = provider
            .get_block_by_number(height.into())
            .await?
            .context("missing confirmed native state")?;
        let pinned = BlockId::hash_canonical(safe.header.hash);
        if chain.get_total_batches_proved(pinned).await? < self.number {
            return Ok(NativeObservation::Pending);
        }
        anyhow::ensure!(
            U256::from(provider.get_chain_id().await?) == self.settlement_chain_id
                && keccak256(provider.get_code_at(config.gate).block_id(pinned).await?)
                    == config.gate_code_hash
                && gate.chain().block(pinned).call().await? == self.chain
                && gate.childChainId().block(pinned).call().await? == self.child_chain_id
                && gate.policyHash().block(pinned).call().await? == config.policy_hash
                && gate.timelock().block(pinned).call().await? == publication.timelock,
            "native settlement differs from reviewed chain and service gate"
        );
        if gate.observedNativeBatch().block(pinned).call().await? < self.number {
            return Ok(NativeObservation::Pending);
        }
        // A fresh gate may be deployed after proving has already begun, including an earlier
        // transaction in the deployment block. Its immutable frontier identifies pre-service work.
        let (observation_height, event_block) = if self.number
            <= gate.initialNativeBatch().block(pinned).call().await?
        {
            (height, safe.header.hash)
        } else {
            let deployment = provider.deployment_block(config.gate).await?;
            anyhow::ensure!(
                deployment <= height,
                "service gate deployment is not confirmed"
            );
            let (mut lo, mut hi) = (deployment, height);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if gate.observedNativeBatch().block(mid.into()).call().await? >= self.number {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            let first_observation = provider
                .get_block_by_number(lo.into())
                .await?
                .context("missing historical gate observation")?;
            let event_block = first_observation.header.hash;
            let filter = Filter::new()
                .address(config.gate)
                .event_signature(IZkSysProofGateV1::NativeSettlementAdvanced::SIGNATURE_HASH)
                .at_block_hash(event_block);
            let mut matched = false;
            for log in provider.get_logs(&filter).await? {
                anyhow::ensure!(
                    !log.removed
                        && log.address() == config.gate
                        && log.block_hash == Some(event_block)
                        && log.block_number == Some(lo)
                        && log.topic0()
                            == Some(&IZkSysProofGateV1::NativeSettlementAdvanced::SIGNATURE_HASH),
                    "noncanonical native settlement event"
                );
                let event =
                    IZkSysProofGateV1::NativeSettlementAdvanced::decode_log(&log.inner)?.data;
                if event.from <= self.number && self.number <= event.to {
                    anyhow::ensure!(
                        !matched
                            && chain.stored_batch_hash(event.to, pinned).await?
                                == event.nativeBatchHash,
                        "native settlement event differs from canonical stored batch"
                    );
                    matched = true;
                }
            }
            if !matched {
                return Ok(NativeObservation::NoGap);
            }
            (lo, event_block)
        };
        anyhow::ensure!(
            chain.stored_batch_hash(self.number, pinned).await? == self.hash,
            "stored native batch {} differs from local proof metadata",
            self.number
        );
        let historical = provider
            .get_block_by_number(observation_height.into())
            .await?
            .context("gate observation disappeared")?;
        let canonical = provider
            .get_block_by_number(height.into())
            .await?
            .context("confirmed native state disappeared")?;
        anyhow::ensure!(
            historical.header.hash == event_block
                && canonical.header.hash == safe.header.hash
                && U256::from(provider.get_chain_id().await?) == self.settlement_chain_id,
            "native settlement changed during confirmation"
        );
        Ok(NativeObservation::Confirmed(event_block))
    }
}
