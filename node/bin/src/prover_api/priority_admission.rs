use crate::config::ServicePublicationConfig;
use crate::prover_api::service_publication::{atomic_write, private_directory, read_private};
use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::providers::Provider as _;
use alloy::rpc::types::BlockId;
use alloy::sol_types::SolValue;
use anyhow::Context as _;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use zksync_os_contract_interface::prover_service_v1::{
    IZkSysPriorityCommitAdmissionV1, PriorityCommitReservationV1,
};
use zksync_os_l1_sender::commands::{L1SenderCommand, commit::CommitCommand};
use zksync_os_l1_sender::config::ConfirmationPolicy;
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{ComponentId, PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_provider::NodeProvider;
use zksync_os_sequencer::model::blocks::PriorityFormation;
use zksync_os_storage_api::{FinalityStatus, ReadReplay};
use zksync_os_types::ZkEnvelope;

#[derive(Clone, Debug)]
pub(crate) struct PriorityAdmission {
    provider: NodeProvider,
    chain: Address,
    admission: Address,
    confirmations: ConfirmationPolicy,
    finality: watch::Receiver<FinalityStatus>,
    directory: PathBuf,
    poll_interval: Duration,
    rpc_timeout: Duration,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FormationRecord {
    chain: Address,
    admission: Address,
    reservation_id: u64,
    reservation_hash: B256,
    batch_number: u64,
    block_number: u64,
}

#[derive(Serialize)]
struct CommitCandidate {
    schema_version: u32,
    chain: Address,
    admission: Address,
    reservation_id: u64,
    batch_number: u64,
    block_number: u64,
    stored_hash: B256,
    priority_hash: B256,
    item_hashes: Vec<B256>,
}

impl PriorityAdmission {
    pub(crate) fn new(
        config: &ServicePublicationConfig,
        provider: NodeProvider,
        chain: Address,
        admission: Address,
        confirmations: ConfirmationPolicy,
        finality: watch::Receiver<FinalityStatus>,
    ) -> anyhow::Result<Self> {
        let directory = config.directory.join("priority-admission");
        private_directory(&directory)?;
        Ok(Self {
            provider,
            chain,
            admission,
            confirmations,
            finality,
            directory,
            poll_interval: config.poll_interval,
            rpc_timeout: config.rpc_timeout,
        })
    }

    async fn anchor(&self) -> anyhow::Result<BlockId> {
        let tip = self.provider.get_block_number().await?;
        let number = self.confirmations.safe_state_block(tip);
        let block = self
            .provider
            .get_block_by_number(number.into())
            .await?
            .context("missing confirmed priority admission block")?;
        Ok(BlockId::hash_canonical(block.header.hash))
    }

    async fn reservation(&self) -> anyhow::Result<PriorityCommitReservationV1> {
        let admission = IZkSysPriorityCommitAdmissionV1::new(self.admission, self.provider.clone());
        Ok(admission
            .reservation()
            .block(self.anchor().await?)
            .call()
            .await?)
    }

    pub(crate) async fn wait_for_formation(
        &self,
        replay: &impl ReadReplay,
        last_issued_batch: Option<u64>,
    ) -> anyhow::Result<PriorityFormation> {
        loop {
            let committed = self.finality.borrow().clone();
            if replay.latest_record() == committed.last_committed_block
                && last_issued_batch.is_none_or(|batch| batch <= committed.last_committed_batch)
            {
                match tokio::time::timeout(self.rpc_timeout, self.reservation()).await {
                    Ok(Ok(reservation))
                        if reservation.id != 0
                            && committed.last_committed_batch.checked_add(1)
                                == Some(reservation.batchNumber)
                            && self.finality.borrow().last_committed_batch
                                == committed.last_committed_batch
                            && replay.latest_record() == committed.last_committed_block =>
                    {
                        let block_number = committed
                            .last_committed_block
                            .checked_add(1)
                            .context("priority admission block number overflow")?;
                        let record = FormationRecord {
                            chain: self.chain,
                            admission: self.admission,
                            reservation_id: reservation.id,
                            reservation_hash: keccak256(reservation.abi_encode()),
                            batch_number: reservation.batchNumber,
                            block_number,
                        };
                        self.store_formation(&record)?;
                        return Ok(PriorityFormation {
                            batch_number: record.batch_number,
                            block_number,
                            priority_from: reservation.priorityFrom,
                            required_end: reservation.requiredEnd,
                            reachable_end: reservation.rootTotal.min(reservation.localTotal),
                        });
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => tracing::warn!(%err, "priority reservation unavailable"),
                    Err(_) => tracing::warn!("timed out reading priority reservation"),
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    fn read_formation(&self) -> anyhow::Result<Option<FormationRecord>> {
        read_private(&self.directory.join("formation.json"))?
            .map(|bytes| serde_json::from_slice(&bytes).context("invalid saved priority formation"))
            .transpose()
    }

    fn store_formation(&self, candidate: &FormationRecord) -> anyhow::Result<()> {
        if let Some(retained) = self.read_formation()? {
            anyhow::ensure!(
                retained.chain == candidate.chain && retained.admission == candidate.admission,
                "priority admission configuration changed with a retained formation record"
            );
            if retained == *candidate {
                return Ok(());
            }
        }
        atomic_write(
            &self.directory.join("formation.json"),
            &serde_json::to_vec(candidate)?,
        )
    }
}

pub(crate) struct PriorityCommitGate<Replay> {
    pub admission: PriorityAdmission,
    pub replay: Replay,
}

#[async_trait]
impl<Replay: ReadReplay> PipelineComponent for PriorityCommitGate<Replay> {
    type Input = L1SenderCommand<CommitCommand>;
    type Output = Self::Input;

    const COMPONENT_ID: ComponentId = ComponentId::PriorityCommitGate;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(command) = input.recv_and_record_picked(&state_reporter).await {
            if let L1SenderCommand::SendToL1(commit) = &command {
                self.wait_until_ready(commit, &state_reporter).await?;
            }
            output.send_and_record(command, &state_reporter).await?;
        }
        Ok(())
    }
}

impl<Replay: ReadReplay> PriorityCommitGate<Replay> {
    async fn wait_until_ready(
        &self,
        command: &CommitCommand,
        state_reporter: &ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let metadata = &command.input.batch;
        let stored = metadata.batch_info.clone().into_stored();
        let formation = self
            .admission
            .read_formation()?
            .context("priority formation record missing")?;
        anyhow::ensure!(
            formation.chain == self.admission.chain
                && formation.admission == self.admission.admission
                && formation.batch_number == stored.batch_number
                && formation.block_number == metadata.first_block_number
                && formation.block_number == metadata.last_block_number,
            "batch is outside its reserved priority formation boundary"
        );
        let replay = self
            .replay
            .get_replay_record(formation.block_number)
            .context("reserved block missing from canonical replay storage")?;
        let item_hashes: Vec<_> = replay
            .transactions
            .into_iter()
            .filter_map(|tx| {
                if let ZkEnvelope::L1(priority) = tx.into_envelope() {
                    Some(*priority.hash())
                } else {
                    None
                }
            })
            .collect();
        anyhow::ensure!(
            u64::try_from(item_hashes.len())? == stored.number_of_layer1_txs,
            "reserved block priority count differs from the native commitment"
        );
        let candidate = CommitCandidate {
            schema_version: 1,
            chain: formation.chain,
            admission: formation.admission,
            reservation_id: formation.reservation_id,
            batch_number: formation.batch_number,
            block_number: formation.block_number,
            stored_hash: stored.hash(),
            priority_hash: stored.priority_operations_hash,
            item_hashes,
        };
        atomic_write(
            &self.admission.directory.join("candidate.json"),
            &serde_json::to_vec(&candidate)?,
        );

        state_reporter.enter_state(GenericComponentState::Idle);
        let policy = IZkSysPriorityCommitAdmissionV1::new(
            self.admission.admission,
            self.admission.provider.clone(),
        );
        loop {
            let result = tokio::time::timeout(self.admission.rpc_timeout, async {
                let anchor = self.admission.anchor().await?;
                let reservation = policy.reservation().block(anchor).call().await?;
                if reservation.id != candidate.reservation_id
                    || keccak256(reservation.abi_encode()) != formation.reservation_hash
                {
                    return Ok::<_, anyhow::Error>(None);
                }
                Ok(Some(
                    policy
                        .commitReady(
                            candidate.batch_number,
                            candidate.stored_hash,
                            U256::from(stored.number_of_layer1_txs),
                            candidate.priority_hash,
                        )
                        .block(anchor)
                        .call()
                        .await?,
                ))
            })
            .await;
            match result {
                Ok(Ok(Some(true))) => {
                    state_reporter.enter_state(GenericComponentState::Active);
                    return Ok(());
                }
                Ok(Ok(Some(false))) => {}
                Ok(Ok(None)) => anyhow::bail!(
                    "confirmed priority reservation changed for batch {}; reconcile the retained formation before resuming",
                    candidate.batch_number
                ),
                Ok(Err(err)) => tracing::warn!(%err, batch = candidate.batch_number,
                    "cannot confirm priority commitment admission"),
                Err(_) => tracing::warn!(
                    batch = candidate.batch_number,
                    "timed out confirming priority commitment admission"
                ),
            }
            tokio::time::sleep(self.admission.poll_interval).await;
        }
    }
}
