//! Regrouping changes local wrapper ownership. Its journal entry never advances a batch
//! or grants service credit, even if a relay transaction was already signed.
use super::*;
use std::collections::BTreeSet;
use zksync_os_contract_interface::ZkChain;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Regrouped {
    schema_version: u32,
    work_hash: B256,
    native_proved: u64,
    native_batch_hash: B256,
}

impl Journal {
    pub(super) fn work_directory(&self, work: &Work) -> PathBuf {
        self.directory.join(format!(
            "{}-{}-{}-{:x}",
            work.execution_chain_id,
            work.batch_from,
            work.batch_to,
            keccak256(&work.proof_data)
        ))
    }

    pub(super) fn entries(&self, max_records: usize) -> anyhow::Result<Vec<fs::DirEntry>> {
        let entries = fs::read_dir(&self.directory)?
            .take(max_records + 2)
            .collect::<Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            entries.len() <= max_records + 1,
            "service publication journal is over capacity"
        );
        Ok(entries
            .into_iter()
            .filter(|entry| entry.file_name() != ".lock")
            .collect())
    }

    pub(super) fn regrouped(
        &self,
        directory: &Path,
        work: &Work,
        work_hash: B256,
    ) -> anyhow::Result<Option<Regrouped>> {
        let Some(bytes) = read_private(&directory.join("regrouped.json"))? else {
            return Ok(None);
        };
        let marker: Regrouped = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            marker.schema_version == 1
                && marker.work_hash == work_hash
                && marker
                    .native_proved
                    .checked_add(1)
                    .is_some_and(|next| next < work.batch_from)
                && !marker.native_batch_hash.is_zero()
                && directory == self.work_directory(work),
            "regrouping disposition differs from retained service work"
        );
        Ok(Some(marker))
    }
}

impl ServicePublication {
    pub(in crate::prover_api) fn regrouped_candidates(
        &self,
    ) -> anyhow::Result<BTreeSet<(u64, u64)>> {
        let mut ranges = BTreeSet::new();
        for entry in self.journal.entries(self.config.max_records)? {
            let directory = entry.path();
            private_directory(&directory)?;
            if !directory.join("regrouped.json").try_exists()? {
                continue;
            }
            let bytes = read_private(&directory.join("work.json"))?
                .context("regrouping marker without service work")?;
            let work: Work = serde_json::from_slice(&bytes)?;
            if self
                .journal
                .regrouped(&directory, &work, keccak256(&bytes))?
                .is_some()
                && read_private(&directory.join("confirmed.json"))?.is_none()
            {
                ranges.insert((work.batch_from, work.batch_to));
            }
        }
        Ok(ranges)
    }

    pub(in crate::prover_api) async fn is_regrouped(
        &self,
        command: &ProofCommand,
    ) -> anyhow::Result<bool> {
        let work = Work::new(command, self)?;
        let directory = self.journal.work_directory(&work);
        if !directory.join("regrouped.json").try_exists()? {
            return Ok(false);
        }
        let (retained, directory, work_hash) =
            self.journal.stage(&work, self.config.max_records)?;
        let Some(marker) = self.journal.regrouped(&directory, &retained, work_hash)? else {
            return Ok(false);
        };
        if read_private(&directory.join("confirmed.json"))?.is_some() {
            return Ok(false);
        }
        let (latest, _, hash) = self
            .regrouping_anchor(&retained, marker.native_proved)
            .await?;
        anyhow::ensure!(
            latest < work.batch_from && hash == marker.native_batch_hash,
            "regrouped wrapper or its historical native anchor changed during recovery"
        );
        Ok(true)
    }

    pub(in crate::prover_api) async fn regroup_unproved(
        &self,
        command: &ProofCommand,
        native_proved: u64,
    ) -> anyhow::Result<()> {
        let work = Work::new(command, self)?;
        anyhow::ensure!(
            native_proved
                .checked_add(1)
                .is_some_and(|next| next < work.batch_from),
            "regrouping requires an unproved gap before the retained wrapper"
        );
        let (work, directory, work_hash) = self.journal.stage(&work, self.config.max_records)?;
        let (latest, _, native_batch_hash) = self.regrouping_anchor(&work, native_proved).await?;
        anyhow::ensure!(
            latest == native_proved,
            "native frontier changed before regrouping"
        );
        anyhow::ensure!(
            read_private(&directory.join("confirmed.json"))?.is_none()
                && read_private(&directory.join("superseded.json"))?.is_none(),
            "cannot regroup terminal service work"
        );
        let marker = Regrouped {
            schema_version: 1,
            work_hash,
            native_proved,
            native_batch_hash,
        };
        if let Some(previous) = self.journal.regrouped(&directory, &work, work_hash)? {
            anyhow::ensure!(previous == marker, "regrouping disposition changed");
        } else {
            atomic_write(
                &directory.join("regrouped.json"),
                &serde_json::to_vec(&marker)?,
            )?;
        }
        Ok(())
    }

    async fn regrouping_anchor(
        &self,
        work: &Work,
        native_proved: u64,
    ) -> anyhow::Result<(u64, u64, B256)> {
        let provider = &self.provider;
        let policy = ConfirmationPolicy::new(
            work.required_confirmations
                .max(self.confirmation_policy.required_confirmations()),
        );
        let snapshot = super::super::snark_proving_pipeline_step::canonical_proved_frontiers(
            provider,
            work.chain_address,
            work.settlement_chain_id.try_into()?,
            policy,
        )
        .await?;
        anyhow::ensure!(
            snapshot.confirmation_safe_proved >= native_proved,
            "confirmed native frontier precedes regrouping anchor"
        );
        let pinned = BlockId::hash_canonical(snapshot.safe_hash);
        let gate = IZkSysProofGateV1::new(work.gate, provider.clone());
        anyhow::ensure!(
            keccak256(provider.get_code_at(work.gate).block_id(pinned).await?)
                == work.gate_code_hash
                && gate.chain().block(pinned).call().await? == work.chain_address
                && gate.childChainId().block(pinned).call().await? == work.execution_chain_id,
            "reviewed native chain differs from service work"
        );
        let batch_hash = ZkChain::new(work.chain_address, provider.clone())
            .stored_batch_hash(native_proved, pinned)
            .await?;
        anyhow::ensure!(!batch_hash.is_zero(), "missing native regrouping anchor");
        Ok((
            snapshot.latest_proved,
            snapshot.confirmation_safe_proved,
            batch_hash,
        ))
    }

    pub(super) async fn reactivate_if_next(
        &self,
        work: &Work,
        directory: &Path,
        work_hash: B256,
    ) -> anyhow::Result<()> {
        let Some(marker) = self.journal.regrouped(directory, work, work_hash)? else {
            return Ok(());
        };
        loop {
            let (latest, safe, hash) = self.regrouping_anchor(work, marker.native_proved).await?;
            anyhow::ensure!(
                hash == marker.native_batch_hash,
                "regenerated work has a different native anchor"
            );
            if latest.checked_add(1) != Some(work.batch_from) {
                break;
            }
            if safe == latest {
                fs::remove_file(directory.join("regrouped.json"))?;
                File::open(directory)?.sync_all()?;
                break;
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
        Ok(())
    }

    pub(super) async fn has_acceptance_hint(
        &self,
        directory: &Path,
        work: &Work,
        work_hash: B256,
    ) -> anyhow::Result<bool> {
        if let Some(bytes) = read_private(&directory.join("confirmed.json"))? {
            let confirmed: Confirmed = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                confirmed.schema_version == 1 && confirmed.work_hash == work_hash,
                "confirmed disposition differs from service work"
            );
            return Ok(true);
        }
        let Some(bytes) = read_private(&directory.join("relay.json"))? else {
            return Ok(false);
        };
        let handoff: Handoff = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            handoff.schema_version == 1
                && handoff.work_hash == work_hash
                && !handoff.transaction_hash.is_zero(),
            "service handoff identity mismatch"
        );
        let Some(receipt) = self
            .provider
            .get_transaction_receipt(handoff.transaction_hash)
            .await?
        else {
            return Ok(false);
        };
        let package_hash = hash_package(&handoff.sidecar.accepted_package);
        Ok(receipt.status()
            && receipt.inner.logs().iter().any(|log| {
                log.address() == work.gate
                    && !log.removed
                    && log.transaction_hash == Some(handoff.transaction_hash)
                    && IZkSysProofGateV1::ServicePackageAccepted::decode_log(&log.inner).is_ok_and(
                        |event| {
                            event.data.packageHash == package_hash
                                && event.data.from == work.batch_from
                                && event.data.to == work.batch_to
                        },
                    )
            }))
    }
}
