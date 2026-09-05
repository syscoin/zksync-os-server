//! SYSCOIN: real Raft + replay RocksDB crash boundaries. Seals are synthetic because
//! these tests authenticate durable replay contents, not execution or header validity.

use super::*;
use crate::storage::{RaftColumnFamily, RaftLogStore};
use alloy::primitives::{B256, Sealed};
use std::path::Path;
use zksync_os_storage::db::BlockReplayStorage;
use zksync_os_storage_api::{BlockContext, WriteReplay};
use zksync_os_types::{BlockStartCursors, ProtocolSemanticVersion};

fn record(number: u64, marker: u8) -> ReplayRecord {
    ReplayRecord {
        block_context: BlockContext {
            chain_id: 270,
            block_number: number,
            timestamp: 1000 + number,
            ..Default::default()
        },
        transactions: vec![],
        previous_block_timestamp: if number == 0 { 0 } else { 999 + number },
        node_version: "0.1.0".parse().unwrap(),
        protocol_version: ProtocolSemanticVersion::canonical_genesis_version(),
        block_output_hash: B256::repeat_byte(marker),
        force_preimages: vec![],
        starting_cursors: BlockStartCursors::default(),
    }
}

fn log_id(index: u64, term: u64) -> LogId<PeerId> {
    // Distinct terms also cover replacements canonized following a leadership change.
    LogId::new(openraft::CommittedLeaderId::new(term, PeerId::ZERO), index)
}

fn open(
    path: &Path,
) -> (
    BlockReplayStorage,
    RaftLogStore,
    RaftStateMachineStore,
    mpsc::UnboundedReceiver<ReplayRecord>,
) {
    let wal = BlockReplayStorage::new_without_genesis(&path.join("wal"), 270);
    let log = RaftLogStore::open(&path.join("raft")).unwrap();
    let (sender, receiver) = mpsc::unbounded_channel();
    let machine = RaftStateMachineStore::new(log.db(), Box::new(wal.clone()), sender);
    (wal, log, machine, receiver)
}

async fn persist(wal: &BlockReplayStorage, record: ReplayRecord, replacement: bool) {
    let seal = record.block_output_hash;
    wal.write(Sealed::new_unchecked(record, seal), replacement)
        .await
        .unwrap();
}

async fn forward(
    machine: &mut RaftStateMachineStore,
    receiver: &mut mpsc::UnboundedReceiver<ReplayRecord>,
    record: &ReplayRecord,
    log_id: LogId<PeerId>,
) {
    machine
        .apply([Entry {
            log_id,
            payload: EntryPayload::Normal(record.clone()),
        }])
        .await
        .unwrap();
    assert_eq!(receiver.recv().await.unwrap(), *record);
}

async fn initialize(path: &Path, tail: u64) {
    let (wal, _, mut machine, mut receiver) = open(path);
    persist(&wal, record(0, 0), false).await;
    for number in 1..=tail {
        let record = record(number, number as u8);
        forward(&mut machine, &mut receiver, &record, log_id(number, 1)).await;
        persist(&wal, record, false).await;
    }
    assert_eq!(
        machine.applied_state().await.unwrap().0,
        Some(log_id(tail, 1))
    );
}

async fn assert_reopened_applied(path: &Path, expected: LogId<PeerId>) {
    let (wal, log, mut machine, _) = open(path);
    // SYSCOIN: operator startup diagnostics and OpenRaft must use the same identity check.
    assert_eq!(
        log.startup_state(&wal).unwrap().durable_applied,
        Some(expected)
    );
    assert_eq!(machine.applied_state().await.unwrap().0, Some(expected));
}

#[tokio::test]
async fn append_crash_before_and_after_wal_write() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let next = record(2, 2);
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &next, log_id(2, 1)).await;
        // Drop every DB handle before reopening: forwarding is not a WAL write.
    }
    assert_reopened_applied(dir.path(), log_id(1, 1)).await;
    {
        let (wal, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &next, log_id(2, 1)).await;
        persist(&wal, next, false).await;
    }
    assert_reopened_applied(dir.path(), log_id(2, 1)).await;
}

#[tokio::test]
async fn same_height_rebuild_crash_retains_original_log_id() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let replacement = record(1, 2);
    {
        let (wal, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &replacement, log_id(2, 2)).await;
        assert_eq!(wal.latest_record(), 1);
        assert_eq!(
            wal.get_replay_record_identity(1),
            Some(record(1, 1).consensus_identity())
        );
    }
    assert_reopened_applied(dir.path(), log_id(1, 1)).await;
    {
        let (wal, _, mut machine, mut receiver) = open(dir.path());
        // OpenRaft's restarted reapplication must preserve the old identity and baseline.
        forward(&mut machine, &mut receiver, &replacement, log_id(2, 2)).await;
        assert_eq!(machine.applied_state().await.unwrap().0, Some(log_id(1, 1)));
        persist(&wal, replacement, true).await;
    }
    assert_reopened_applied(dir.path(), log_id(2, 2)).await;
}

#[tokio::test]
async fn repeated_same_height_rebuilds_and_leadership_changes() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let mut previous = log_id(1, 1);
    // Return to the first exact identity after two replacements; retaining only the
    // latest mapping by height or destructively updating a digest key both lose history.
    for (offset, marker) in [2, 3, 1, 4].into_iter().enumerate() {
        let next_id = log_id(offset as u64 + 2, offset as u64 + 2);
        let replacement = record(1, marker);
        {
            let (_, _, mut machine, mut receiver) = open(dir.path());
            forward(&mut machine, &mut receiver, &replacement, next_id).await;
        }
        assert_reopened_applied(dir.path(), previous).await;
        {
            let (wal, _, mut machine, mut receiver) = open(dir.path());
            forward(&mut machine, &mut receiver, &replacement, next_id).await;
            persist(&wal, replacement, true).await;
        }
        assert_reopened_applied(dir.path(), next_id).await;
        previous = next_id;
    }
}

#[tokio::test]
async fn partial_rebuild_tracks_identity_below_old_wal_tip() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 2).await;
    let boundary = record(1, 3);
    let tail = record(2, 4);
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &boundary, log_id(3, 2)).await;
        forward(&mut machine, &mut receiver, &tail, log_id(4, 2)).await;
    }
    assert_reopened_applied(dir.path(), log_id(2, 1)).await;
    {
        let (wal, _, _, _) = open(dir.path());
        persist(&wal, boundary, true).await;
        assert_eq!(wal.latest_record(), 2);
    }
    assert_reopened_applied(dir.path(), log_id(3, 2)).await;
    {
        let (wal, _, _, _) = open(dir.path());
        persist(&wal, tail, true).await;
    }
    assert_reopened_applied(dir.path(), log_id(4, 2)).await;
}

#[tokio::test]
async fn matching_pending_tail_does_not_skip_unwritten_boundary() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 2).await;
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &record(1, 3), log_id(3, 2)).await;
        // This pending record equals the OLD tail exactly. Picking the greatest matching
        // per-record LogId would falsely skip log 3, whose boundary is still unwritten.
        forward(&mut machine, &mut receiver, &record(2, 2), log_id(4, 2)).await;
    }
    assert_reopened_applied(dir.path(), log_id(2, 1)).await;
}

#[tokio::test]
async fn equivalent_complete_final_state_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        forward(&mut machine, &mut receiver, &record(1, 2), log_id(2, 2)).await;
        forward(&mut machine, &mut receiver, &record(1, 1), log_id(3, 2)).await;
    }
    // SYSCOIN: A -> B -> A has exactly the final canonical replay state already durable
    // as A. The journal authenticates that state; it is not a count of physical writes.
    assert_reopened_applied(dir.path(), log_id(3, 2)).await;
}

#[tokio::test]
async fn membership_survives_crash_while_rebuild_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        machine
            .apply([Entry {
                log_id: log_id(2, 2),
                payload: EntryPayload::Membership(Default::default()),
            }])
            .await
            .unwrap();
        forward(&mut machine, &mut receiver, &record(1, 2), log_id(3, 2)).await;
    }
    let (_, _, mut machine, _) = open(dir.path());
    let (applied, membership) = machine.applied_state().await.unwrap();
    assert_eq!(applied, Some(log_id(1, 1)));
    assert_eq!(*membership.log_id(), Some(log_id(2, 2)));
}

#[tokio::test]
async fn payload_changes_with_same_output_hash_are_not_durable() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    {
        let (_, _, mut machine, mut receiver) = open(dir.path());
        let mut replacement = record(1, 1);
        replacement
            .force_preimages
            .push((B256::repeat_byte(7), vec![1, 2, 3]));
        forward(&mut machine, &mut receiver, &replacement, log_id(2, 2)).await;
    }
    assert_reopened_applied(dir.path(), log_id(1, 1)).await;
}

#[tokio::test]
async fn unexpected_wal_mutation_has_no_authenticated_prefix() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    {
        let (wal, _, _, _) = open(dir.path());
        persist(&wal, record(1, 99), true).await;
    }
    let (wal, log, mut machine, _) = open(dir.path());
    assert!(log.startup_state(&wal).is_err());
    assert!(machine.applied_state().await.is_err());
}

#[tokio::test]
async fn reapplication_cannot_replace_journal_identity() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let (_, _, mut machine, _) = open(dir.path());
    let result = machine
        .apply([Entry {
            log_id: log_id(1, 1),
            payload: EntryPayload::Normal(record(1, 99)),
        }])
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn journal_rejects_new_out_of_order_log_index() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let (_, _, mut machine, _) = open(dir.path());
    assert!(
        machine
            .apply([Entry {
                log_id: log_id(0, 1),
                payload: EntryPayload::Normal(record(1, 99)),
            }])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_journal_baseline_and_version_fail_closed() {
    for corruption in 0..6 {
        let dir = tempfile::tempdir().unwrap();
        initialize(dir.path(), 1).await;
        {
            let (_, log, _, _) = open(dir.path());
            let db = log.db();
            let (cf, key): (_, Vec<u8>) = match corruption {
                0 | 3 | 5 => (
                    RaftColumnFamily::AppliedJournal,
                    1u64.to_be_bytes().to_vec(),
                ),
                1 | 4 => (
                    RaftColumnFamily::AppliedBaseline,
                    1u64.to_be_bytes().to_vec(),
                ),
                2 => (
                    RaftColumnFamily::StateMachineMeta,
                    b"syscoin_applied_journal_version".to_vec(),
                ),
                _ => unreachable!(),
            };
            let mut value = db.get_cf(cf, &key).unwrap().unwrap();
            let mut batch = db.new_write_batch();
            match corruption {
                0..=2 => {
                    value.push(0); // Valid bincode prefix followed by garbage is not accepted.
                    batch.put_cf(cf, &key, &value);
                }
                3 => {
                    batch.delete_cf(cf, &key);
                    batch.put_cf(cf, &2u64.to_be_bytes(), &value); // Key disagrees with LogId.
                }
                4 => {
                    batch.delete_cf(cf, &key);
                    batch.put_cf(cf, &[1], &value); // Not a u64 baseline key.
                }
                5 => batch.put_cf(cf, &key, &[]), // Truncated journal value.
                _ => unreachable!(),
            }
            db.write(batch).unwrap();
        }
        // Version corruption fails during open; row corruption fails the startup scan.
        if let Ok(log) = RaftLogStore::open(&dir.path().join("raft")) {
            let wal = BlockReplayStorage::new_without_genesis(&dir.path().join("wal"), 270);
            assert!(log.startup_state(&wal).is_err(), "corruption={corruption}");
            let (sender, _) = mpsc::unbounded_channel();
            let mut machine = RaftStateMachineStore::new(log.db(), Box::new(wal), sender);
            assert!(
                machine.applied_state().await.is_err(),
                "corruption={corruption}"
            );
        }
    }
}

#[derive(Debug)]
struct MissingWalIdentity(BlockReplayStorage);

impl ReadReplay for MissingWalIdentity {
    fn get_context(&self, block_number: u64) -> Option<BlockContext> {
        self.0.get_context(block_number)
    }

    fn get_original_context(&self, block_number: u64) -> Option<BlockContext> {
        self.0.get_original_context(block_number)
    }

    fn get_replay_record_identity(&self, _block_number: u64) -> Option<B256> {
        None
    }

    fn get_replay_record_by_key(
        &self,
        block_number: u64,
        db_key: Option<Vec<u8>>,
    ) -> Option<ReplayRecord> {
        self.0.get_replay_record_by_key(block_number, db_key)
    }

    fn get_canonical_block_hash(&self, block_number: u64) -> Option<B256> {
        self.0.get_canonical_block_hash(block_number)
    }

    fn latest_record(&self) -> u64 {
        self.0.latest_record()
    }
}

#[tokio::test]
async fn missing_wal_identity_is_not_reconstructed_or_assumed_applied() {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path(), 1).await;
    let wal = BlockReplayStorage::new_without_genesis(&dir.path().join("wal"), 270);
    let log = RaftLogStore::open(&dir.path().join("raft")).unwrap();
    let missing = MissingWalIdentity(wal);
    assert!(log.startup_state(&missing).is_err());
    let (sender, _) = mpsc::unbounded_channel();
    let mut machine = RaftStateMachineStore::new(log.db(), Box::new(missing), sender);
    assert!(machine.applied_state().await.is_err());
}

#[tokio::test]
async fn direct_state_machine_constructor_rejects_legacy_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let wal = BlockReplayStorage::new_without_genesis(&dir.path().join("wal"), 270);
    persist(&wal, record(0, 0), false).await;
    let db = RocksDB::<RaftColumnFamily>::new(&dir.path().join("raft"))
        .unwrap()
        .with_sync_writes();
    let mut batch = db.new_write_batch();
    let value = bincode::serde::encode_to_vec(log_id(1, 1), bincode::config::standard()).unwrap();
    batch.put_cf(RaftColumnFamily::RaftApplied, &1u64.to_be_bytes(), &value);
    db.write(batch).unwrap();
    let (sender, _) = mpsc::unbounded_channel();
    let mut machine = RaftStateMachineStore::new(db, Box::new(wal), sender);
    assert!(machine.applied_state().await.is_err());
    assert!(
        machine
            .apply([Entry {
                log_id: log_id(2, 1),
                payload: EntryPayload::Normal(record(1, 2)),
            }])
            .await
            .is_err()
    );
}

#[test]
fn legacy_height_only_applied_metadata_is_explicitly_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raft");
    {
        let store = RaftLogStore::open(&path).unwrap();
        let db = store.db();
        let mut batch = db.new_write_batch();
        let value =
            bincode::serde::encode_to_vec(log_id(1, 1), bincode::config::standard()).unwrap();
        batch.put_cf(RaftColumnFamily::RaftApplied, &1u64.to_be_bytes(), &value);
        db.write(batch).unwrap();
    }
    let error = RaftLogStore::open(&path).unwrap_err().to_string();
    assert!(error.contains("legacy height-only"), "{error}");
}
