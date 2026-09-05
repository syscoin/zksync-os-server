//! OpenRaft state-machine implementation for replay-record based application state.
//!
//! `apply()` is invoked when a Raft log is canonized (accepted by a quorum):
//! - `Blank` entries are acknowledged immediately.
//! - `Membership` entries are saved to the meta store synchronously (eagerly, mid-batch)
//!   so they are not lost if the process crashes before the batch is fully applied.
//! - SYSCOIN: `Normal(ReplayRecord)` entries journal their log index and immutable replay
//!   identity **before** forwarding to the pipeline. Startup authenticates the resulting
//!   state of a journal prefix against the durable WAL, including same-height rebuilds.
//!

use crate::storage::{RaftStateMachineMetaStore, io_err, io_err_msg};
use openraft::storage::{RaftSnapshotBuilder, RaftStateMachine as RaftStateMachineTrait};
use openraft::{
    Entry, EntryPayload, ErrorSubject, ErrorVerb, LogId, Snapshot, SnapshotMeta, StorageError,
    StoredMembership,
};
use reth_network_peers::PeerId;
use tokio::sync::mpsc;
use zksync_os_consensus_types::{RaftNode, RaftTypeConfig, debug_display_raft_entry};
use zksync_os_rocksdb::RocksDB;
use zksync_os_storage_api::{ReadReplay, ReplayRecord};

#[derive(Debug)]
pub struct RaftStateMachineStore {
    pub(crate) meta_store: RaftStateMachineMetaStore,
    /// Unbounded to avoid deadlock during `reapply_committed()` at startup,
    /// which runs inside `Raft::new()` before the pipeline is consuming from the other end.
    pub(crate) applied_sender: mpsc::UnboundedSender<ReplayRecord>,
    /// SYSCOIN: read-only WAL identities authenticate the durable journal prefix.
    pub(crate) wal: Box<dyn ReadReplay>,
}

impl RaftStateMachineStore {
    /// Constructs state-machine store using raft DB handle, a WAL reference, and
    /// the apply-forwarding channel sender.
    pub fn new(
        db: RocksDB<crate::storage::RaftColumnFamily>,
        wal: Box<dyn ReadReplay>,
        applied_sender: mpsc::UnboundedSender<ReplayRecord>,
    ) -> Self {
        Self {
            meta_store: RaftStateMachineMetaStore::new(db),
            applied_sender,
            wal,
        }
    }
}

impl RaftStateMachineTrait<RaftTypeConfig> for RaftStateMachineStore {
    type SnapshotBuilder = NoopSnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<PeerId>>, StoredMembership<PeerId, RaftNode>), StorageError<PeerId>>
    {
        let meta = self.meta_store.load(ErrorSubject::StateMachine)?;
        let membership = meta
            .last_membership
            .unwrap_or_else(|| StoredMembership::new(None, Default::default()));

        // SYSCOIN: height alone is not an acknowledgement: a rebuilt block reuses it,
        // and an in-progress rebuild can leave the WAL tip above the applied replacement.
        let durable = self.meta_store.durable_applied_state(self.wal.as_ref())?;
        if durable.pending_records != 0 {
            tracing::info!(
                last_applied = ?durable.last_applied,
                last_forwarded = ?durable.last_forwarded,
                pending_records = durable.pending_records,
                "Raft replay journal is ahead of durable WAL identities; OpenRaft will reapply pending records",
            );
        }
        Ok((durable.last_applied, membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<()>, StorageError<PeerId>>
    where
        I: IntoIterator<Item = Entry<RaftTypeConfig>> + Send,
        I::IntoIter: Send,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        tracing::debug!(
            "applying {} entries: {}",
            entries.len(),
            entries
                .iter()
                .map(debug_display_raft_entry)
                .collect::<Vec<_>>()
                .join(", "),
        );
        let mut responses = Vec::new();

        for entry in &entries {
            match &entry.payload {
                EntryPayload::Blank => responses.push(()),
                EntryPayload::Normal(data) => {
                    // SYSCOIN: preserve the exact identity and superseded LogIds before
                    // forwarding. A channel send is not a durable application acknowledgement.
                    self.meta_store
                        .save_record_log_id(data, entry.log_id, self.wal.as_ref())?;

                    if let Err(error) = self.applied_sender.send(data.clone()) {
                        tracing::warn!("raft applied channel closed: {error}");
                        return Err(io_err(
                            &ErrorSubject::StateMachine,
                            ErrorVerb::Write,
                            &error,
                        ));
                    }
                    responses.push(());
                }
                EntryPayload::Membership(membership) => {
                    // Save membership eagerly rather than batching with other entries.
                    // If we saved it only at the end of the batch and crashed mid-batch,
                    // the membership change would be lost on restart (OpenRaft would not
                    // re-apply it because `applied_state()` returns a durable prefix's LogId
                    // which may already be past this entry).
                    let mut meta = self.meta_store.load(ErrorSubject::StateMachine)?;
                    meta.last_membership = Some(StoredMembership::new(
                        Some(entry.log_id),
                        membership.clone(),
                    ));
                    self.meta_store.save(&meta, ErrorSubject::StateMachine)?;
                    tracing::debug!("membership change persisted: log_id={:?}", entry.log_id);
                    responses.push(());
                }
            }
        }

        tracing::debug!("{} entries applied", entries.len());
        Ok(responses)
    }

    // Rest of the file only contains functions related to snapshots.
    // We don't use the openraft's snapshot feature, so implementations are stubs and can be ignored.

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        NoopSnapshotBuilder
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<<RaftTypeConfig as openraft::RaftTypeConfig>::SnapshotData>, StorageError<PeerId>>
    {
        Err(io_err_msg(
            &ErrorSubject::StateMachine,
            ErrorVerb::Read,
            "snapshotting disabled",
        ))
    }

    async fn install_snapshot(
        &mut self,
        _meta: &SnapshotMeta<PeerId, RaftNode>,
        _snapshot: Box<<RaftTypeConfig as openraft::RaftTypeConfig>::SnapshotData>,
    ) -> Result<(), StorageError<PeerId>> {
        Err(io_err_msg(
            &ErrorSubject::StateMachine,
            ErrorVerb::Write,
            "snapshotting disabled",
        ))
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<RaftTypeConfig>>, StorageError<PeerId>> {
        Ok(None)
    }
}

#[derive(Debug, Clone)]
/// Snapshot builder placeholder; snapshots are intentionally disabled.
pub struct NoopSnapshotBuilder;

impl RaftSnapshotBuilder<RaftTypeConfig> for NoopSnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<RaftTypeConfig>, StorageError<PeerId>> {
        Err(io_err_msg(
            &ErrorSubject::StateMachine,
            ErrorVerb::Write,
            "snapshotting disabled",
        ))
    }
}

#[cfg(test)]
mod syscoin_rebuild_tests;
