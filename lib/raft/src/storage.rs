//! Raft log storage low-level primitives backed by RocksDB.
//!
//! This module implements log-side OpenRaft storage (`RaftLogStorage` / `RaftLogReader`).
//! It also owns low-level state-machine metadata persistence primitives that are
//! consumed by `state_machine.rs`.

use alloy::primitives::B256;
use openraft::storage::{LogFlushed, LogState, RaftLogReader, RaftLogStorage};
use openraft::{
    AnyError, Entry, ErrorSubject, ErrorVerb, LogId, StorageError, StorageIOError,
    StoredMembership, Vote,
};
use reth_network_peers::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::ops::RangeBounds;
use std::path::Path;
use zksync_os_consensus_types::{RaftNode, RaftTypeConfig};
use zksync_os_rocksdb::RocksDB;
use zksync_os_rocksdb::db::NamedColumnFamily;
use zksync_os_storage_api::{ReadReplay, ReplayRecord};

#[derive(Clone, Debug)]
pub struct RaftLogStore {
    db: RocksDB<RaftColumnFamily>,
}

#[derive(Clone, Debug)]
pub(crate) struct RaftStateMachineMetaStore {
    db: RocksDB<RaftColumnFamily>,
}

#[derive(Copy, Clone, Debug)]
pub enum RaftColumnFamily {
    /// Raft log entries.
    Logs,
    /// Persisted vote.
    Vote,
    /// Log metadata (`committed`).
    LogMeta,
    /// State-machine metadata (last membership).
    StateMachineMeta,
    /// Legacy height-only applied map. SYSCOIN: rejected, never silently migrated.
    RaftApplied,
    /// SYSCOIN: append-only, log-index-keyed replay identities, including superseded rebuilds.
    AppliedJournal,
    /// SYSCOIN: WAL identity before the journal first touches each block number.
    AppliedBaseline,
}

impl NamedColumnFamily for RaftColumnFamily {
    const DB_NAME: &'static str = "raft";
    const ALL: &'static [Self] = &[
        RaftColumnFamily::Logs,
        RaftColumnFamily::Vote,
        RaftColumnFamily::LogMeta,
        RaftColumnFamily::StateMachineMeta,
        RaftColumnFamily::RaftApplied,
        RaftColumnFamily::AppliedJournal,
        RaftColumnFamily::AppliedBaseline,
    ];

    fn name(&self) -> &'static str {
        match self {
            RaftColumnFamily::Logs => "logs",
            RaftColumnFamily::Vote => "vote",
            RaftColumnFamily::LogMeta => "log_meta",
            RaftColumnFamily::StateMachineMeta => "state_machine_meta",
            RaftColumnFamily::RaftApplied => "raft_applied",
            RaftColumnFamily::AppliedJournal => "syscoin_applied_journal_v1",
            RaftColumnFamily::AppliedBaseline => "syscoin_applied_baseline_v1",
        }
    }
}

pub(crate) fn io_err<E: std::error::Error + 'static>(
    subject: &ErrorSubject<PeerId>,
    verb: ErrorVerb,
    err: &E,
) -> StorageError<PeerId> {
    StorageError::IO {
        source: StorageIOError::new(subject.clone(), verb, AnyError::new(err)),
    }
}

#[allow(clippy::result_large_err)]
fn db_get<T: for<'de> serde::Deserialize<'de>>(
    db: &RocksDB<RaftColumnFamily>,
    cf: RaftColumnFamily,
    key: &[u8],
    subject: &ErrorSubject<PeerId>,
) -> Result<Option<T>, StorageError<PeerId>> {
    let Some(bytes) = db
        .get_cf(cf, key)
        .map_err(|e| io_err(subject, ErrorVerb::Read, &e))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_exact(&bytes, subject)?))
}

// SYSCOIN: versioned metadata must not accept a valid prefix of another/corrupt schema.
#[allow(clippy::result_large_err)]
fn decode_exact<T: for<'de> serde::Deserialize<'de>>(
    bytes: &[u8],
    subject: &ErrorSubject<PeerId>,
) -> Result<T, StorageError<PeerId>> {
    let (decoded, consumed) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map_err(|error| io_err(subject, ErrorVerb::Read, &error))?;
    if consumed != bytes.len() {
        return Err(io_err_msg(
            subject,
            ErrorVerb::Read,
            "trailing bytes in Raft storage metadata",
        ));
    }
    Ok(decoded)
}

#[allow(clippy::result_large_err)]
fn db_put<T: serde::Serialize>(
    db: &RocksDB<RaftColumnFamily>,
    cf: RaftColumnFamily,
    key: &[u8],
    value: &T,
    subject: &ErrorSubject<PeerId>,
) -> Result<(), StorageError<PeerId>> {
    let encoded =
        bincode::serde::encode_to_vec(value, bincode::config::standard()).expect("bincode encode");
    let mut batch = db.new_write_batch();
    batch.put_cf(cf, key, &encoded);
    db.write(batch)
        .map_err(|e| io_err(subject, ErrorVerb::Write, &e))
}

pub(crate) fn io_err_msg(
    subject: &ErrorSubject<PeerId>,
    verb: ErrorVerb,
    msg: impl ToString,
) -> StorageError<PeerId> {
    StorageError::IO {
        source: StorageIOError::new(subject.clone(), verb, AnyError::error(msg)),
    }
}

/// Snapshot of the raw Raft storage state captured before `Raft::new()` runs.
///
#[derive(Debug)]
pub struct RaftStorageStartupState {
    /// Last `Vote` persisted to the Vote CF (the node this peer voted for and in which term).
    pub vote: Option<Vote<PeerId>>,
    /// Last committed `LogId` persisted to the LogMeta CF.
    pub committed: Option<LogId<PeerId>>,
    /// `LogId` of the last entry in the Logs CF (may be ahead of `committed` if a leader
    /// wrote entries that were never committed before crashing).
    pub last_log: Option<LogId<PeerId>>,
    /// SYSCOIN: the last journal prefix authenticated by the durable WAL identities.
    /// This is exactly what `applied_state()` returns on this startup.
    /// Any committed entries with index > this value will be reapplied by `Raft::new()`.
    pub durable_applied: Option<LogId<PeerId>>,
}

impl RaftLogStore {
    /// Opens raft storage DB with sync writes enabled.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let db = RocksDB::<RaftColumnFamily>::new(path)
            .map_err(|e| anyhow::anyhow!("opening raft db at {}: {e}", path.display()))?
            .with_sync_writes();
        let meta_store = RaftStateMachineMetaStore::new(db.clone());
        meta_store.ensure_applied_journal_schema()?;
        Ok(Self { db })
    }

    /// Returns a clone of the underlying raft RocksDB handle.
    pub(crate) fn db(&self) -> RocksDB<RaftColumnFamily> {
        self.db.clone()
    }

    /// Reads the raw storage state that `Raft::new()` will use to initialise itself.
    #[allow(clippy::result_large_err)]
    pub fn startup_state(
        &self,
        wal: &dyn ReadReplay,
    ) -> Result<RaftStorageStartupState, StorageError<PeerId>> {
        let vote = db_get(
            &self.db,
            RaftColumnFamily::Vote,
            Self::VOTE_KEY,
            &ErrorSubject::Store,
        )?;
        let committed = db_get(
            &self.db,
            RaftColumnFamily::LogMeta,
            Self::COMMITTED_KEY,
            &ErrorSubject::Store,
        )?;
        let last_log = self.last_log_id_from_db()?;
        let durable_applied = RaftStateMachineMetaStore::new(self.db.clone())
            .durable_applied_state(wal)?
            .last_applied;
        Ok(RaftStorageStartupState {
            vote,
            committed,
            last_log,
            durable_applied,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct RaftStateMachineMeta {
    pub(crate) last_membership: Option<StoredMembership<PeerId, RaftNode>>,
}

// SYSCOIN: The WAL's block height is not an application generation. Preserve every
// forwarded identity so a same-height rebuild cannot overwrite its predecessor's LogId.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct AppliedJournalEntry {
    log_id: LogId<PeerId>,
    block_number: u64,
    identity: B256,
}

#[derive(Debug)]
pub(crate) struct DurableAppliedState {
    pub(crate) last_applied: Option<LogId<PeerId>>,
    pub(crate) last_forwarded: Option<LogId<PeerId>>,
    pub(crate) pending_records: usize,
}

impl RaftStateMachineMetaStore {
    const STATE_MACHINE_META_KEY: &'static [u8] = b"state_machine_meta";
    const APPLIED_JOURNAL_VERSION_KEY: &'static [u8] = b"syscoin_applied_journal_version";
    const APPLIED_JOURNAL_VERSION: u32 = 1;

    pub(crate) fn new(db: RocksDB<RaftColumnFamily>) -> Self {
        Self { db }
    }

    #[allow(clippy::result_large_err)]
    pub(crate) fn load(
        &self,
        subject: ErrorSubject<PeerId>,
    ) -> Result<RaftStateMachineMeta, StorageError<PeerId>> {
        Ok(db_get(
            &self.db,
            RaftColumnFamily::StateMachineMeta,
            Self::STATE_MACHINE_META_KEY,
            &subject,
        )?
        .unwrap_or_default())
    }

    // SYSCOIN: This unreleased storage format has no implicit migration: legacy applied
    // maps have already discarded the old LogIds needed to authenticate rebuilt heights.
    // Guessing from those rows or from the current WAL would recreate the crash bug.
    #[allow(clippy::result_large_err)]
    fn ensure_applied_journal_schema(&self) -> Result<(), StorageError<PeerId>> {
        if self
            .db
            .from_iterator_cf(RaftColumnFamily::RaftApplied, &[]..)
            .next()
            .is_some()
        {
            return Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Read,
                "SYSCOIN: legacy height-only Raft applied metadata is unsupported; use an explicitly coordinated fresh Raft history, not an automatic migration",
            ));
        }
        let version: Option<u32> = db_get(
            &self.db,
            RaftColumnFamily::StateMachineMeta,
            Self::APPLIED_JOURNAL_VERSION_KEY,
            &ErrorSubject::StateMachine,
        )?;
        if let Some(version) = version {
            return if version == Self::APPLIED_JOURNAL_VERSION {
                Ok(())
            } else {
                Err(io_err_msg(
                    &ErrorSubject::StateMachine,
                    ErrorVerb::Read,
                    format!("unsupported SYSCOIN Raft applied journal version {version}"),
                ))
            };
        }
        for cf in [
            RaftColumnFamily::AppliedJournal,
            RaftColumnFamily::AppliedBaseline,
        ] {
            if self.db.from_iterator_cf(cf, &[]..).next().is_some() {
                return Err(io_err_msg(
                    &ErrorSubject::StateMachine,
                    ErrorVerb::Read,
                    "SYSCOIN Raft applied journal has no format version",
                ));
            }
        }
        db_put(
            &self.db,
            RaftColumnFamily::StateMachineMeta,
            Self::APPLIED_JOURNAL_VERSION_KEY,
            &Self::APPLIED_JOURNAL_VERSION,
            &ErrorSubject::StateMachine,
        )
    }

    /// SYSCOIN: persist the immutable replay identity before forwarding to the pipeline.
    /// The baseline and first journal entry for a height are one synced RocksDB write.
    /// Reapplication is idempotent and must not replace either an identity or its baseline.
    #[allow(clippy::result_large_err)]
    pub(crate) fn save_record_log_id(
        &self,
        record: &ReplayRecord,
        log_id: LogId<PeerId>,
        wal: &dyn ReadReplay,
    ) -> Result<(), StorageError<PeerId>> {
        // Also validate direct state-machine construction, which can bypass LogStore::open.
        self.ensure_applied_journal_schema()?;
        let entry = AppliedJournalEntry {
            log_id,
            block_number: record.block_context.block_number,
            identity: record.consensus_identity(),
        };
        let journal_key = log_id.index.to_be_bytes();
        let existing: Option<AppliedJournalEntry> = db_get(
            &self.db,
            RaftColumnFamily::AppliedJournal,
            &journal_key,
            &ErrorSubject::StateMachine,
        )?;
        if let Some(existing) = existing {
            return if existing == entry {
                Ok(())
            } else {
                Err(io_err_msg(
                    &ErrorSubject::StateMachine,
                    ErrorVerb::Write,
                    "SYSCOIN: attempted to change an existing Raft applied journal entry",
                ))
            };
        }
        if let Some((last_key, _)) = self
            .db
            .to_iterator_cf(
                RaftColumnFamily::AppliedJournal,
                ..=&u64::MAX.to_be_bytes()[..],
            )
            .next()
            && last_key.as_ref() >= journal_key.as_slice()
        {
            return Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Write,
                "SYSCOIN: out-of-order Raft applied journal entry",
            ));
        }
        let block_key = entry.block_number.to_be_bytes();
        let baseline: Option<Option<B256>> = db_get(
            &self.db,
            RaftColumnFamily::AppliedBaseline,
            &block_key,
            &ErrorSubject::StateMachine,
        )?;
        let mut batch = self.db.new_write_batch();
        if baseline.is_none() {
            let identity = Self::wal_identity(wal, entry.block_number)?;
            let encoded = bincode::serde::encode_to_vec(identity, bincode::config::standard())
                .expect("bincode encode WAL baseline");
            batch.put_cf(RaftColumnFamily::AppliedBaseline, &block_key, &encoded);
        }
        let encoded = bincode::serde::encode_to_vec(entry, bincode::config::standard())
            .expect("bincode encode applied journal entry");
        batch.put_cf(RaftColumnFamily::AppliedJournal, &journal_key, &encoded);
        self.db
            .write(batch)
            .map_err(|error| io_err(&ErrorSubject::StateMachine, ErrorVerb::Write, &error))
    }

    #[allow(clippy::result_large_err)]
    fn wal_identity(
        wal: &dyn ReadReplay,
        block_number: u64,
    ) -> Result<Option<B256>, StorageError<PeerId>> {
        let identity = wal.get_replay_record_identity(block_number);
        if identity.is_none() && block_number <= wal.latest_record() {
            return Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Read,
                format!(
                    "SYSCOIN: WAL block {block_number} has no immutable replay identity; legacy or incomplete WAL is unsupported"
                ),
            ));
        }
        Ok(identity)
    }

    /// SYSCOIN: authenticate a *prefix*, not merely the highest matching record.
    /// During a multi-block rebuild the WAL tip can still contain an old tail. A later
    /// pending entry can even equal that old tail while an earlier replacement is not
    /// persisted. Only a prefix whose final identity at EVERY touched height matches the
    /// WAL is durable. Superseded entries remain in the journal for repeated rebuilds.
    ///
    /// Equivalent complete final states (including an A -> B -> A cycle) are idempotent;
    /// this authenticates canonical replay state, not the occurrence of each transient write.
    /// The scan takes O(journal entries + touched heights) time and O(touched heights) memory.
    /// It runs at startup, not per block; snapshots/purging/checkpoint compaction are disabled.
    #[allow(clippy::result_large_err)]
    pub(crate) fn durable_applied_state(
        &self,
        wal: &dyn ReadReplay,
    ) -> Result<DurableAppliedState, StorageError<PeerId>> {
        self.ensure_applied_journal_schema()?;
        let mut identities = HashMap::new();
        let mut mismatches = 0usize;
        for (key, value) in self
            .db
            .from_iterator_cf(RaftColumnFamily::AppliedBaseline, &[]..)
        {
            let number = u64::from_be_bytes(key.as_ref().try_into().map_err(|_| {
                io_err_msg(
                    &ErrorSubject::StateMachine,
                    ErrorVerb::Read,
                    "invalid Raft applied baseline key",
                )
            })?);
            let baseline: Option<B256> = decode_exact(&value, &ErrorSubject::StateMachine)?;
            let actual = Self::wal_identity(wal, number)?;
            mismatches += usize::from(baseline != actual);
            identities.insert(number, (baseline, actual, false));
        }
        let mut matched_prefix = mismatches == 0;
        let mut state = DurableAppliedState {
            last_applied: None,
            last_forwarded: None,
            pending_records: 0,
        };
        for (key, value) in self
            .db
            .from_iterator_cf(RaftColumnFamily::AppliedJournal, &[]..)
        {
            let entry: AppliedJournalEntry = decode_exact(&value, &ErrorSubject::StateMachine)?;
            if key.as_ref() != entry.log_id.index.to_be_bytes() {
                return Err(io_err_msg(
                    &ErrorSubject::StateMachine,
                    ErrorVerb::Read,
                    "Raft applied journal key does not match LogId",
                ));
            }
            let (expected, actual, touched) =
                identities.get_mut(&entry.block_number).ok_or_else(|| {
                    io_err_msg(
                        &ErrorSubject::StateMachine,
                        ErrorVerb::Read,
                        "Raft applied journal entry has no baseline",
                    )
                })?;
            mismatches -= usize::from(*expected != *actual);
            *expected = Some(entry.identity);
            *touched = true;
            mismatches += usize::from(*expected != *actual);
            state.last_forwarded = Some(entry.log_id);
            state.pending_records += 1;
            if mismatches == 0 {
                matched_prefix = true;
                state.last_applied = Some(entry.log_id);
                state.pending_records = 0;
            }
        }
        if identities.values().any(|(_, _, touched)| !touched) {
            return Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Read,
                "Raft applied baseline has no journal entry",
            ));
        }
        if !matched_prefix {
            return Err(io_err_msg(
                &ErrorSubject::StateMachine,
                ErrorVerb::Read,
                "SYSCOIN: WAL identities do not match any Raft applied journal prefix; refusing to guess durable application",
            ));
        }
        Ok(state)
    }

    #[allow(clippy::result_large_err)]
    pub(crate) fn save(
        &self,
        meta: &RaftStateMachineMeta,
        subject: ErrorSubject<PeerId>,
    ) -> Result<(), StorageError<PeerId>> {
        db_put(
            &self.db,
            RaftColumnFamily::StateMachineMeta,
            Self::STATE_MACHINE_META_KEY,
            meta,
            &subject,
        )
    }
}

impl RaftLogStore {
    const COMMITTED_KEY: &'static [u8] = b"committed";
    const VOTE_KEY: &'static [u8] = b"vote";

    fn index_key(index: u64) -> [u8; 8] {
        index.to_be_bytes()
    }

    fn decode_entry(bytes: &[u8]) -> Result<Entry<RaftTypeConfig>, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    fn encode_entry(entry: &Entry<RaftTypeConfig>) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(entry)
    }

    #[allow(clippy::result_large_err)]
    fn last_log_id_from_db(&self) -> Result<Option<LogId<PeerId>>, StorageError<PeerId>> {
        let end = u64::MAX.to_be_bytes();
        let mut iter = self
            .db
            .to_iterator_cf(RaftColumnFamily::Logs, ..=end.as_slice());
        if let Some((_key, value)) = iter.next() {
            let entry = Self::decode_entry(&value)
                .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?;
            Ok(Some(entry.log_id))
        } else {
            Ok(None)
        }
    }
}

impl RaftLogReader<RaftTypeConfig> for RaftLogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<RaftTypeConfig>>, StorageError<PeerId>> {
        let start = match range.start_bound() {
            std::ops::Bound::Included(v) => *v,
            std::ops::Bound::Excluded(v) => v.saturating_add(1),
            std::ops::Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            std::ops::Bound::Included(v) => Some(*v),
            std::ops::Bound::Excluded(v) => v.checked_sub(1),
            std::ops::Bound::Unbounded => None,
        };

        let mut entries = Vec::new();
        let start_key = Self::index_key(start);
        for (key, value) in self
            .db
            .from_iterator_cf(RaftColumnFamily::Logs, start_key.as_slice()..)
        {
            let index = u64::from_be_bytes(key.as_ref().try_into().map_err(|_| {
                io_err_msg(
                    &ErrorSubject::Store,
                    ErrorVerb::Read,
                    "invalid raft log index key",
                )
            })?);
            if let Some(end) = end
                && index > end
            {
                break;
            }
            let entry = Self::decode_entry(&value)
                .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

impl RaftLogStorage<RaftTypeConfig> for RaftLogStore {
    type LogReader = RaftLogStore;

    async fn get_log_state(&mut self) -> Result<LogState<RaftTypeConfig>, StorageError<PeerId>> {
        let last_log_id = self.last_log_id_from_db()?;
        Ok(LogState {
            // Purging/snapshotting is intentionally disabled.
            last_purged_log_id: None,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<PeerId>) -> Result<(), StorageError<PeerId>> {
        let bytes = bincode::serde::encode_to_vec(vote, bincode::config::standard())
            .expect("serialize vote");
        let mut batch = self.db.new_write_batch();
        batch.put_cf(RaftColumnFamily::Vote, Self::VOTE_KEY, &bytes);
        self.db
            .write(batch)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<PeerId>>, StorageError<PeerId>> {
        let bytes = self
            .db
            .get_cf(RaftColumnFamily::Vote, Self::VOTE_KEY)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let vote = bincode::serde::decode_from_slice::<Vote<PeerId>, _>(
            &bytes,
            bincode::config::standard(),
        )
        .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?
        .0;
        Ok(Some(vote))
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<PeerId>>,
    ) -> Result<(), StorageError<PeerId>> {
        let mut batch = self.db.new_write_batch();
        if let Some(committed) = committed {
            let encoded = bincode::serde::encode_to_vec(committed, bincode::config::standard())
                .expect("serialize committed log id");
            batch.put_cf(RaftColumnFamily::LogMeta, Self::COMMITTED_KEY, &encoded);
        } else {
            batch.delete_cf(RaftColumnFamily::LogMeta, Self::COMMITTED_KEY);
        }
        self.db
            .write(batch)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
        Ok(())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<PeerId>>, StorageError<PeerId>> {
        let bytes = self
            .db
            .get_cf(RaftColumnFamily::LogMeta, Self::COMMITTED_KEY)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let committed = bincode::serde::decode_from_slice::<LogId<PeerId>, _>(
            &bytes,
            bincode::config::standard(),
        )
        .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Read, &e))?
        .0;
        Ok(Some(committed))
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<RaftTypeConfig>,
    ) -> Result<(), StorageError<PeerId>>
    where
        I: IntoIterator<Item = Entry<RaftTypeConfig>> + Send,
        I::IntoIter: Send,
    {
        let mut batch = self.db.new_write_batch();
        for entry in entries {
            let key = Self::index_key(entry.log_id.index);
            let value = Self::encode_entry(&entry)
                .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
            batch.put_cf(RaftColumnFamily::Logs, &key, &value);
        }
        self.db
            .write(batch)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<PeerId>) -> Result<(), StorageError<PeerId>> {
        let start_key = Self::index_key(log_id.index);
        let mut batch = self.db.new_write_batch();
        for (key, _value) in self
            .db
            .from_iterator_cf(RaftColumnFamily::Logs, start_key.as_slice()..)
        {
            batch.delete_cf(RaftColumnFamily::Logs, &key);
        }
        self.db
            .write(batch)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<PeerId>) -> Result<(), StorageError<PeerId>> {
        let mut batch = self.db.new_write_batch();
        let start = Self::index_key(0);
        let end = Self::index_key(log_id.index.saturating_add(1));
        batch.delete_range_cf(RaftColumnFamily::Logs, start.as_slice()..end.as_slice());
        self.db
            .write(batch)
            .map_err(|e| io_err(&ErrorSubject::Store, ErrorVerb::Write, &e))?;
        Ok(())
    }
}
