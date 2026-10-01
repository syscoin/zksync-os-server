//! Durable handoff to a service wallet. Files are hints; only canonical settlement receipts
//! for the exact native command authorize downstream batch execution.
use crate::config::ServicePublicationConfig;
use alloy::{
    consensus::Transaction as _,
    network::TransactionResponse as _,
    primitives::{Address, B256, Bytes, U256, keccak256},
    providers::Provider,
    rpc::types::{BlockId, BlockNumberOrTag},
    sol_types::{SolCall, SolEvent, SolValue},
};
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::mpsc;
use zksync_os_batch_types::batcher_model::{FriProof, SignedBatchEnvelope};
use zksync_os_contract_interface::prover_service_v1::{
    BatchOutput, IZkSysProofGateV1, IZkSysWrapperCoordinatorV1, PackagePhaseV1,
    ProverServiceSidecarV1, StoredBatch, ValidationContextV1, hash_package,
};
use zksync_os_l1_sender::{
    commands::{L1SenderCommand, SendToL1, prove::ProofCommand},
    config::ConfirmationPolicy,
    pipeline_component::L1Sender,
};
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};
use zksync_os_provider::NodeProvider;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) enum ProofPublication {
    Ordinary(L1Sender<ProofCommand>),
    Service(ServicePublication),
}

#[derive(Clone)]
pub(crate) struct ServicePublication {
    pub config: ServicePublicationConfig,
    pub provider: NodeProvider,
    pub confirmation_policy: ConfirmationPolicy,
    pub timelock: Address,
    journal: Arc<Journal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
    schema_version: u32,
    execution_chain_id: U256,
    settlement_chain_id: U256,
    chain_address: Address,
    gate: Address,
    gate_code_hash: B256,
    coordinator: Address,
    coordinator_code_hash: B256,
    policy_hash: B256,
    production_vk_hash: B256,
    sequencer: Address,
    timelock: Address,
    required_confirmations: u64,
    batch_from: u64,
    batch_to: u64,
    proof_data: Bytes,
    batch_outputs: Vec<BatchOutput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    schema_version: u32,
    work_hash: B256,
    transaction_hash: B256,
    sidecar: ProverServiceSidecarV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmed {
    schema_version: u32,
    work_hash: B256,
    transaction_hash: B256,
    block_hash: B256,
    block_number: u64,
    calldata_hash: B256,
    package_hash: B256,
}

impl Work {
    fn new(command: &ProofCommand, publication: &ServicePublication) -> anyhow::Result<Self> {
        let batches = command.as_ref();
        let first = batches
            .first()
            .context("empty service publication command")?;
        let commit = &first.batch.batch_info.commit_info;
        let config = &publication.config;
        let proof_data = command.native_proof_data()?;
        anyhow::ensure!(
            proof_data.first() == Some(&1),
            "unsupported native proof encoding"
        );
        let (_, _, proof) =
            <(StoredBatch, Vec<StoredBatch>, Vec<U256>)>::abi_decode_params_validate(
                &proof_data[1..],
            )?;
        anyhow::ensure!(
            proof.len() == 46 && proof[0] == U256::from(0x802) && proof[1].is_zero(),
            "service publication requires a real canonical V8 proof"
        );

        let work = Self {
            schema_version: 1,
            execution_chain_id: U256::from(commit.chain_id),
            settlement_chain_id: U256::from(commit.sl_chain_id),
            chain_address: first.batch.chain_address,
            gate: config.gate,
            gate_code_hash: config.gate_code_hash,
            coordinator: config.coordinator,
            coordinator_code_hash: config.coordinator_code_hash,
            policy_hash: config.policy_hash,
            production_vk_hash: config.production_vk_hash,
            sequencer: config.sequencer,
            timelock: publication.timelock,
            required_confirmations: publication.confirmation_policy.required_confirmations(),
            batch_from: first.batch_number(),
            batch_to: batches.last().unwrap().batch_number(),
            proof_data,
            batch_outputs: command.service_batch_outputs(),
        };
        anyhow::ensure!(
            !work.chain_address.is_zero()
                && !work.execution_chain_id.is_zero()
                && !work.settlement_chain_id.is_zero(),
            "service publication requires native chain identity"
        );
        Ok(work)
    }

    fn matches_batch(
        &self,
        batch: &SignedBatchEnvelope<FriProof>,
        publication: &ServicePublication,
    ) -> anyhow::Result<()> {
        let config = &publication.config;
        let native = &batch.batch.batch_info.commit_info;
        anyhow::ensure!(
            self.schema_version == 1
                && self.execution_chain_id == U256::from(native.chain_id)
                && self.settlement_chain_id == U256::from(native.sl_chain_id)
                && self.chain_address == batch.batch.chain_address
                && self.gate == config.gate
                && self.gate_code_hash == config.gate_code_hash
                && self.policy_hash == config.policy_hash
                && self.production_vk_hash == config.production_vk_hash
                && self.sequencer == config.sequencer
                && self.timelock == publication.timelock
                && ((self.coordinator == config.coordinator
                    && self.coordinator_code_hash == config.coordinator_code_hash)
                    || (self.coordinator.is_zero() && self.coordinator_code_hash.is_zero())),
            "retained service work differs from native batch or reviewed configuration"
        );
        anyhow::ensure!(
            self.proof_data.first() == Some(&1),
            "invalid retained native proof data"
        );
        let (_, batches, _) =
            <(StoredBatch, Vec<StoredBatch>, Vec<U256>)>::abi_decode_params_validate(
                &self.proof_data[1..],
            )?;
        let index = usize::try_from(
            batch
                .batch_number()
                .checked_sub(self.batch_from)
                .context("batch precedes work")?,
        )?;
        let stored = batches
            .get(index)
            .context("batch missing from retained native payload")?;
        let expected = zksync_os_contract_interface::IExecutor::StoredBatchInfo::from(
            &batch.batch.batch_info.clone().into_stored(),
        );
        anyhow::ensure!(
            stored.abi_encode() == expected.abi_encode(),
            "passthrough metadata differs from retained native service proof"
        );
        Ok(())
    }

    fn context(&self, sidecar: &ProverServiceSidecarV1) -> anyhow::Result<ValidationContextV1> {
        let accepted = &sidecar.accepted_package;
        let phase = if let Some(candidate) = &sidecar.candidate {
            anyhow::ensure!(
                !self.coordinator.is_zero() && !self.coordinator_code_hash.is_zero(),
                "service publication requires a pinned coordinator"
            );
            let mut normalized = accepted.clone();
            normalized.turn = 0;
            normalized.proofHash = B256::ZERO;
            normalized.wrapper = Address::ZERO;
            normalized.wrapperBeneficiary = Address::ZERO;
            PackagePhaseV1::Service {
                period: accepted.period,
                roster_root: accepted.rosterRoot,
                selected_wrapper_index: candidate.index,
                turn: accepted.turn,
                frozen_package_hash: hash_package(&normalized),
            }
        } else {
            PackagePhaseV1::Bootstrap
        };
        // These historical phase fields only reconstruct calldata. Successful execution by the
        // pinned gate, checked below, authenticates the former live turn and endorsements.
        Ok(ValidationContextV1 {
            child_chain_id: self.execution_chain_id,
            child_chain_address: self.chain_address,
            settlement_chain_id: self.settlement_chain_id,
            policy_hash: self.policy_hash,
            production_vk_hash: self.production_vk_hash,
            sequencer: self.sequencer,
            expected_parent: accepted.parent,
            batch_from: self.batch_from,
            batch_to: self.batch_to,
            phase,
        })
    }
}

#[async_trait]
impl PipelineComponent for ProofPublication {
    type Input = L1SenderCommand<ProofCommand>;
    type Output = SignedBatchEnvelope<FriProof>;
    const COMPONENT_ID: zksync_os_pipeline::ComponentId = <ProofCommand as SendToL1>::COMPONENT_ID;
    const OUTPUT_CHANNEL_CAPACITY: usize = 1;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        let Self::Service(publication) = self else {
            let Self::Ordinary(sender) = self else {
                unreachable!()
            };
            return sender.run(input, output, reporter).await;
        };
        let mut confirmed_work_hashes = std::collections::HashSet::new();
        loop {
            reporter.enter_state(GenericComponentState::Idle);
            let Some(command) = input.recv_and_record_picked(&reporter).await else {
                return Ok(());
            };
            reporter.enter_state(GenericComponentState::Active);
            match command {
                L1SenderCommand::Passthrough(mut batch) => {
                    let work = publication.journal.work_for_batch(&batch, &publication)?;
                    let work_hash = keccak256(serde_json::to_vec(&work)?);
                    if !confirmed_work_hashes.contains(&work_hash) {
                        publication.wait_for_work(&work, None).await?;
                        confirmed_work_hashes.insert(work_hash);
                    }
                    batch.set_stage(ProofCommand::PASSTHROUGH_STAGE);
                    output.send_and_record(*batch, &reporter).await?;
                }
                L1SenderCommand::SendToL1(command) => {
                    publication.confirm_command(&command).await?;
                    command.notify_confirmed();
                    for mut batch in Vec::<SignedBatchEnvelope<FriProof>>::from(command) {
                        batch.set_stage(ProofCommand::MINED_STAGE);
                        output.send_and_record(batch, &reporter).await?;
                    }
                }
            }
        }
    }
}

impl ServicePublication {
    pub(crate) fn new(
        config: ServicePublicationConfig,
        provider: NodeProvider,
        confirmation_policy: ConfirmationPolicy,
        timelock: Address,
    ) -> anyhow::Result<Self> {
        let journal = Arc::new(Journal::open(&config.directory)?);
        Ok(Self {
            config,
            provider,
            confirmation_policy,
            timelock,
            journal,
        })
    }

    /// Startup must authenticate covered wrappers before the ordinary recovery frontier can
    /// retire them. This path never resubmits or recomputes an already accepted native proof.
    pub(super) async fn confirm_command(&self, command: &ProofCommand) -> anyhow::Result<()> {
        let (work, _, _) = self
            .journal
            .stage(&Work::new(command, self)?, self.config.max_records)?;
        self.wait_for_work(&work, Some(command)).await
    }

    async fn wait_for_work(
        &self,
        work: &Work,
        command: Option<&ProofCommand>,
    ) -> anyhow::Result<()> {
        let (_, directory, work_hash) = self.journal.stage(work, self.config.max_records)?;
        tracing::info!(batch_from=work.batch_from, batch_to=work.batch_to,
            directory=%directory.display(), "native proof awaiting canonical service receipt");
        loop {
            private_directory(&directory)?;
            if let Some(hint) = read_private(&directory.join("relay.json"))? {
                let result = async {
                    let handoff: Handoff = serde_json::from_slice(&hint)?;
                    anyhow::ensure!(
                        handoff.schema_version == 1
                            && handoff.work_hash == work_hash
                            && !handoff.transaction_hash.is_zero(),
                        "service handoff identity mismatch"
                    );
                    let context = work.context(&handoff.sidecar)?;
                    let calldata = if let Some(command) = command {
                        command
                            .service_submission(work.gate, &handoff.sidecar, &context)?
                            .calldata
                    } else {
                        handoff.sidecar.validate(&context, &work.proof_data)?;
                        encode_historical_calldata(work, &handoff.sidecar)
                    };
                    self.observe(work, work_hash, &handoff, &calldata).await
                };
                match tokio::time::timeout(self.config.rpc_timeout, result).await {
                    Ok(Ok(Some(confirmed))) => {
                        atomic_write(
                            &directory.join("confirmed.json"),
                            &serde_json::to_vec(&confirmed)?,
                        )?;
                        return Ok(());
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "service receipt check failed; retaining native proof")
                    }
                    Err(_) => {
                        tracing::warn!("service receipt check timed out; retaining native proof")
                    }
                }
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    async fn observe(
        &self,
        work: &Work,
        work_hash: B256,
        handoff: &Handoff,
        calldata: &Bytes,
    ) -> anyhow::Result<Option<Confirmed>> {
        let provider = &self.provider;
        anyhow::ensure!(
            U256::from(provider.get_chain_id().await?) == work.settlement_chain_id,
            "wrong settlement chain for service publication"
        );
        let Some(receipt) = provider
            .get_transaction_receipt(handoff.transaction_hash)
            .await?
        else {
            return Ok(None);
        };
        anyhow::ensure!(receipt.status(), "service transaction reverted");
        let block_number = receipt
            .block_number
            .context("service receipt has no block number")?;
        let block_hash = receipt
            .block_hash
            .context("service receipt has no block hash")?;
        let tip = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("missing settlement tip")?;
        let confirmation_policy = ConfirmationPolicy::new(
            self.confirmation_policy
                .required_confirmations()
                .max(work.required_confirmations),
        );
        if !confirmation_policy.is_confirmed(block_number, tip.header.inner.number) {
            return Ok(None);
        }
        let transaction = provider
            .get_transaction_by_hash(handoff.transaction_hash)
            .await?
            .context("missing accepted transaction")?;
        anyhow::ensure!(
            receipt.transaction_hash == handoff.transaction_hash
                && transaction.tx_hash() == handoff.transaction_hash
                && transaction.block_hash() == Some(block_hash)
                && transaction.block_number() == Some(block_number)
                && transaction.to() == Some(work.gate)
                && receipt.to == Some(work.gate)
                && transaction.input() == calldata
                && transaction.value().is_zero(),
            "service transaction differs from exact native gate call"
        );
        let package_hash = hash_package(&handoff.sidecar.accepted_package);
        let bootstrap = handoff.sidecar.candidate.is_none();
        anyhow::ensure!(
            !bootstrap || work.coordinator.is_zero() && work.coordinator_code_hash.is_zero(),
            "bootstrap work must not pin a future coordinator"
        );
        let mut matched = 0;
        for log in receipt.inner.logs() {
            if log.address() != work.gate
                || log.topic0() != Some(&IZkSysProofGateV1::ServicePackageAccepted::SIGNATURE_HASH)
            {
                continue;
            }
            anyhow::ensure!(
                !log.removed
                    && log.transaction_hash == Some(handoff.transaction_hash)
                    && log.block_hash == Some(block_hash)
                    && log.block_number == Some(block_number),
                "noncanonical service acceptance log"
            );
            let event = IZkSysProofGateV1::ServicePackageAccepted::decode_log(&log.inner)?.data;
            anyhow::ensure!(
                event.packageHash == package_hash
                    && event.from == work.batch_from
                    && event.to == work.batch_to
                    && event.bootstrap == bootstrap,
                "service acceptance event differs from retained package"
            );
            matched += 1;
        }
        anyhow::ensure!(
            matched == 1,
            "expected exactly one service acceptance event"
        );
        let block = BlockId::hash_canonical(block_hash);
        anyhow::ensure!(
            keccak256(provider.get_code_at(work.gate).block_id(block).await?)
                == work.gate_code_hash,
            "service gate runtime does not match reviewed pin"
        );
        let gate = IZkSysProofGateV1::new(work.gate, provider.clone());
        anyhow::ensure!(
            gate.chain().block(block).call().await? == work.chain_address
                && gate.childChainId().block(block).call().await? == work.execution_chain_id
                && gate.timelock().block(block).call().await? == work.timelock
                && gate.policyHash().block(block).call().await? == work.policy_hash
                && gate.productionVkHash().block(block).call().await? == work.production_vk_hash
                && gate.sequencer().block(block).call().await? == work.sequencer
                && (bootstrap || gate.coordinator().block(block).call().await? == work.coordinator),
            "historical service gate configuration differs from native work"
        );
        if !work.coordinator.is_zero() {
            anyhow::ensure!(
                keccak256(
                    provider
                        .get_code_at(work.coordinator)
                        .block_id(block)
                        .await?
                ) == work.coordinator_code_hash,
                "service coordinator runtime does not match reviewed pin"
            );
            let coordinator = IZkSysWrapperCoordinatorV1::new(work.coordinator, provider.clone());
            anyhow::ensure!(
                coordinator.acceptanceGate().block(block).call().await? == work.gate
                    && coordinator.childChainId().block(block).call().await?
                        == work.execution_chain_id
                    && coordinator.childChainAddress().block(block).call().await?
                        == work.chain_address
                    && coordinator.policyHash().block(block).call().await? == work.policy_hash
                    && coordinator.productionVkHash().block(block).call().await?
                        == work.production_vk_hash
                    && coordinator.sequencer().block(block).call().await? == work.sequencer,
                "historical coordinator configuration differs from native work"
            );
        }
        let canonical = provider
            .get_block_by_number(BlockNumberOrTag::Number(block_number))
            .await?
            .context("receipt block disappeared")?;
        let canonical_tip = provider
            .get_block_by_number(BlockNumberOrTag::Number(tip.header.inner.number))
            .await?
            .context("settlement tip disappeared")?;
        let again = provider
            .get_transaction_receipt(handoff.transaction_hash)
            .await?
            .context("service receipt disappeared")?;
        anyhow::ensure!(
            canonical.header.hash == block_hash
                && canonical_tip.header.hash == tip.header.hash
                && again.block_hash == Some(block_hash)
                && again.block_number == Some(block_number)
                && again.status()
                && U256::from(provider.get_chain_id().await?) == work.settlement_chain_id,
            "service receipt changed during canonical confirmation check"
        );
        Ok(Some(Confirmed {
            schema_version: 1,
            work_hash,
            transaction_hash: handoff.transaction_hash,
            block_hash,
            block_number,
            calldata_hash: keccak256(calldata),
            package_hash,
        }))
    }
}

fn encode_historical_calldata(work: &Work, sidecar: &ProverServiceSidecarV1) -> Bytes {
    if let Some(candidate) = &sidecar.candidate {
        IZkSysProofGateV1::submitCall {
            accepted: sidecar.accepted_package.clone(),
            duties: sidecar.duties.clone(),
            outputs: sidecar.batch_outputs.clone(),
            proofData: work.proof_data.clone(),
            candidate: candidate.clone(),
            candidateProof: sidecar.candidate_proof.clone(),
            sequencerSignature: sidecar.sequencer_signature.clone(),
            wrapperSignature: sidecar.wrapper_signature.clone(),
        }
        .abi_encode()
        .into()
    } else {
        IZkSysProofGateV1::submitBootstrapCall {
            accepted: sidecar.accepted_package.clone(),
            duties: sidecar.duties.clone(),
            outputs: sidecar.batch_outputs.clone(),
            proofData: work.proof_data.clone(),
            sequencerSignature: sidecar.sequencer_signature.clone(),
        }
        .abi_encode()
        .into()
    }
}

struct Journal {
    directory: PathBuf,
    _lock: File,
}
impl Journal {
    fn open(directory: &Path) -> anyhow::Result<Self> {
        private_directory(directory)?;
        let path = directory.join(".lock");
        let file = open_private(&path, true)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: The descriptor is live and retained until this journal is dropped.
            anyhow::ensure!(
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
                "service publication journal is already in use"
            );
        }
        Ok(Self {
            directory: directory.to_owned(),
            _lock: file,
        })
    }

    fn work_for_batch(
        &self,
        batch: &SignedBatchEnvelope<FriProof>,
        publication: &ServicePublication,
    ) -> anyhow::Result<Work> {
        let mut found = None;
        let entries = fs::read_dir(&self.directory)?
            .take(publication.config.max_records + 2)
            .collect::<Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            entries.len() <= publication.config.max_records + 1,
            "service publication journal is over capacity"
        );
        for entry in entries {
            if entry.file_name() == ".lock" {
                continue;
            }
            private_directory(&entry.path())?;
            let Some(bytes) = read_private(&entry.path().join("work.json"))? else {
                continue;
            };
            let work: Work = serde_json::from_slice(&bytes)?;
            if batch.batch_number() < work.batch_from || batch.batch_number() > work.batch_to {
                continue;
            }
            work.matches_batch(batch, publication)?;
            anyhow::ensure!(
                found.is_none(),
                "ambiguous service work for passthrough batch"
            );
            found = Some(work);
        }
        found.context("proved service batch has no retained native publication work")
    }

    fn stage(&self, work: &Work, max_records: usize) -> anyhow::Result<(Work, PathBuf, B256)> {
        let encoded = serde_json::to_vec(work)?;
        anyhow::ensure!(
            encoded.len() as u64 <= MAX_FILE_BYTES,
            "service work exceeds durable bound"
        );
        let work_hash = keccak256(&encoded);
        let name = format!(
            "{}-{}-{}-{:x}",
            work.execution_chain_id,
            work.batch_from,
            work.batch_to,
            keccak256(&work.proof_data)
        );
        let directory = self.directory.join(name);
        if !directory.try_exists()? {
            let count = fs::read_dir(&self.directory)?.try_fold(
                0,
                |count, entry| -> std::io::Result<usize> {
                    Ok(count + usize::from(entry?.file_name() != ".lock"))
                },
            )?;
            anyhow::ensure!(
                count < max_records,
                "service publication journal is full; archive completed records"
            );
        }
        private_directory(&directory)?;
        let path = directory.join("work.json");
        if let Some(previous) = read_private(&path)? {
            let retained: Work = serde_json::from_slice(&previous)?;
            let mut comparable = retained.clone();
            if retained.coordinator.is_zero() && retained.coordinator_code_hash.is_zero() {
                comparable.coordinator = work.coordinator;
                comparable.coordinator_code_hash = work.coordinator_code_hash;
            }
            anyhow::ensure!(
                &comparable == work && serde_json::to_vec(&retained)? == previous,
                "retained service work or publication pins changed"
            );
            return Ok((retained, directory, keccak256(previous)));
        } else {
            atomic_write(&path, &encoded)?;
        }
        Ok((work.clone(), directory, work_hash))
    }
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.is_absolute(),
        "service publication path must be absolute"
    );
    // Reject every symlink component, including an otherwise private child reached through one.
    let mut current = PathBuf::new();
    for component in path.components() {
        anyhow::ensure!(
            !matches!(component, std::path::Component::ParentDir),
            "parent path components are forbidden"
        );
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "service directory path contains a symlink or non-directory"
            );
        }
    }
    if !path.try_exists()? {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        #[cfg(not(unix))]
        anyhow::bail!("service publication requires Unix file protections");
        File::open(path.parent().context("missing service directory parent")?)?.sync_all()?;
    }
    verify_private(&fs::symlink_metadata(path)?, true)?;
    Ok(())
}

fn verify_private(metadata: &fs::Metadata, directory: bool) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and only reads this process's credentials.
        let uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            (if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            }) && metadata.uid() == uid
                && metadata.mode() & 0o077 == 0
                && (directory || metadata.nlink() == 1),
            "service handoff must be a private owner-only non-linked file/directory"
        );
        Ok(())
    }
    #[cfg(not(unix))]
    anyhow::bail!("service publication requires Unix file protections")
}

fn open_private(path: &Path, create: bool) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    verify_private(&opened, false)?;
    let linked = fs::symlink_metadata(path)?;
    verify_private(&linked, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            opened.dev() == linked.dev() && opened.ino() == linked.ino(),
            "service file changed while opening"
        );
    }
    Ok(file)
}

fn read_private(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let mut file = match open_private(path, false) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    anyhow::ensure!(
        file.metadata()?.len() <= MAX_FILE_BYTES,
        "service handoff file exceeds bound"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "service handoff file grew beyond bound"
    );
    Ok(Some(bytes))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("missing handoff directory")?;
    verify_private(&fs::symlink_metadata(parent)?, true)?;
    if path.symlink_metadata().is_ok() {
        let _ = open_private(path, false)?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
