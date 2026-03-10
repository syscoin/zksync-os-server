//! OpenRaft state-machine implementation for replay-record based application state.
//!
//! `apply()` is invoked when a Raft log is canonized (accepted by a quorum).
//! - Internal raft log types (`Membership`, `Blank`) are persisted synchronously before returning.
//! - New canonized blocks (`EntryPayload::Normal(ReplayRecord)`) are forwarded to the
//!   downstream pipeline via `applied_sender`; `apply()` does not wait for WAL persistence.
//!

use crate::storage::{RaftStateMachineMetaStore, io_err, io_err_msg};
use openraft::storage::{RaftSnapshotBuilder, RaftStateMachine as RaftStateMachineTrait};
use openraft::{
    Entry, EntryPayload, ErrorSubject, ErrorVerb, LogId, Snapshot, SnapshotMeta, StorageError,
    StoredMembership,
};
use reth_network_peers::PeerId;
use std::future::Future;
use tokio::sync::mpsc;
use zksync_os_consensus_types::{display_raft_entry, RaftNode, RaftTypeConfig};
use zksync_os_rocksdb::RocksDB;
use zksync_os_storage_api::ReplayRecord;

#[derive(Debug)]
pub struct RaftStateMachineStore {
    pub(crate) meta_store: RaftStateMachineMetaStore,
    pub(crate) applied_sender: mpsc::Sender<ReplayRecord>,
}

impl RaftStateMachineStore {
    /// Constructs state-machine store using raft DB handle and apply-forwarding channel.
    pub fn new(
        db: RocksDB<crate::storage::RaftColumnFamily>,
        applied_sender: mpsc::Sender<ReplayRecord>,
    ) -> Self {
        Self {
            meta_store: RaftStateMachineMetaStore::new(db),
            applied_sender,
        }
    }
}

impl RaftStateMachineTrait<RaftTypeConfig> for RaftStateMachineStore {
    type SnapshotBuilder = NoopSnapshotBuilder;

    fn applied_state(
        &mut self,
    ) -> impl Future<
        Output = Result<
            (
                Option<LogId<PeerId>>,
                StoredMembership<PeerId, RaftNode>,
            ),
            StorageError<PeerId>,
        >,
    > + Send {
        async move {
            let meta = self.meta_store.load(ErrorSubject::StateMachine)?;
            let membership = meta
                .last_membership
                .unwrap_or_else(|| StoredMembership::new(None, Default::default()));
            Ok((meta.last_applied_log_id, membership))
        }
    }

    fn apply<I>(
        &mut self,
        entries: I,
    ) -> impl Future<Output = Result<Vec<()>, StorageError<PeerId>>> + Send
    where
        I: IntoIterator<Item = Entry<RaftTypeConfig>> + Send,
        I::IntoIter: Send,
    {
        async move {
            let entries: Vec<_> = entries.into_iter().collect();
            let mut meta = self.meta_store.load(ErrorSubject::StateMachine)?;
            tracing::debug!(
                "applying {} entries: {}. Meta before: {:?}",
                entries.len(),
                entries
                    .iter()
                    .map(display_raft_entry)
                    .collect::<Vec<_>>()
                    .join(", "),
                meta
            );
            let mut responses = Vec::new();

            for entry in &entries {
                meta.last_applied_log_id = Some(entry.log_id);
                match &entry.payload {
                    EntryPayload::Blank => responses.push(()),
                    EntryPayload::Normal(data) => {
                        if let Err(error) = self.applied_sender.send(data.clone()).await {
                            tracing::warn!(%error, "raft applied channel closed");
                            return Err(io_err(
                                &ErrorSubject::StateMachine,
                                ErrorVerb::Write,
                                &error,
                            ));
                        }
                        responses.push(());
                    }
                    EntryPayload::Membership(membership) => {
                        meta.last_membership = Some(StoredMembership::new(
                            Some(entry.log_id),
                            membership.clone(),
                        ));
                        responses.push(());
                    }
                }
            }

            tracing::debug!("{} entries applied. Meta after: {:?}", entries.len(), meta);
            self.meta_store.save(&meta, ErrorSubject::StateMachine)?;
            Ok(responses)
        }
    }

    // Rest of the file only contains functions related to snapshots.
    // We don't use the openraft's snapshot feature, so implementations are stubs and can be ignored.

    fn get_snapshot_builder(&mut self) -> impl Future<Output = Self::SnapshotBuilder> + Send {
        async move { NoopSnapshotBuilder }
    }

    fn begin_receiving_snapshot(
        &mut self,
    ) -> impl Future<
        Output = Result<
            Box<<RaftTypeConfig as openraft::RaftTypeConfig>::SnapshotData>,
            StorageError<PeerId>,
        >,
    > + Send {
        async move {
            Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Read,
                "snapshotting disabled",
            ))
        }
    }

    fn install_snapshot(
        &mut self,
        _meta: &SnapshotMeta<PeerId, RaftNode>,
        _snapshot: Box<<RaftTypeConfig as openraft::RaftTypeConfig>::SnapshotData>,
    ) -> impl Future<Output = Result<(), StorageError<PeerId>>> + Send {
        async move {
            Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Write,
                "snapshotting disabled",
            ))
        }
    }

    fn get_current_snapshot(
        &mut self,
    ) -> impl Future<Output = Result<Option<Snapshot<RaftTypeConfig>>, StorageError<PeerId>>> + Send
    {
        async move { Ok(None) }
    }
}

#[derive(Debug, Clone)]
/// Snapshot builder placeholder; snapshots are intentionally disabled.
pub struct NoopSnapshotBuilder;

impl RaftSnapshotBuilder<RaftTypeConfig> for NoopSnapshotBuilder {
    fn build_snapshot(
        &mut self,
    ) -> impl Future<Output = Result<Snapshot<RaftTypeConfig>, StorageError<PeerId>>> + Send {
        async move {
            Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Write,
                "snapshotting disabled",
            ))
        }
    }
}
