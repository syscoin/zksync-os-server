//! SYSCOIN: Restart/recovery regressions over the real replay WAL and canonical patched VM.
//! Synthetic seals isolate rebuild ancestry; archive tests use actual VM header/output hashes.
use super::replay_wal_is_linked_from;
use alloy::primitives::{Address, B256, BlockHash, Sealed, U256};
use zksync_os_storage::db::BlockReplayStorage;
use zksync_os_storage_api::{BlockContext, BlockHashes, ReadReplay, ReplayRecord, WriteReplay};
use zksync_os_types::{BlockStartCursors, ProtocolSemanticVersion};

fn block_hash(number: u64) -> BlockHash {
    B256::from(U256::from(0xff_0000 + number))
}

fn chain(len: u64) -> Vec<Sealed<ReplayRecord>> {
    let mut records: Vec<Sealed<ReplayRecord>> = Vec::new();
    for number in 0..len {
        let (block_hashes, previous_block_timestamp) =
            records
                .last()
                .map_or((BlockHashes::default(), 0), |parent| {
                    (
                        parent.block_context.block_hashes.push(parent.hash()),
                        parent.block_context.timestamp,
                    )
                });
        records.push(Sealed::new_unchecked(
            ReplayRecord {
                block_context: BlockContext {
                    chain_id: 270,
                    block_number: number,
                    block_hashes,
                    timestamp: 1_000 + number,
                    gas_limit: 100_000_000,
                    ..Default::default()
                },
                transactions: vec![],
                previous_block_timestamp,
                node_version: "0.1.0".parse().unwrap(),
                protocol_version: ProtocolSemanticVersion::canonical_genesis_version(),
                block_output_hash: B256::from(U256::from(0xb0_0000 + number)),
                force_preimages: vec![],
                starting_cursors: BlockStartCursors::default(),
            },
            block_hash(number),
        ));
    }
    records
}

// SYSCOIN: Exercise the actual command-source startup, including an initially empty async
// canonizer bridge after Raft has durably applied only the rebuilt boundary block.
async fn partial_raft_resume(leader: bool, changed_boundary: bool, reset_timestamps: bool) {
    use super::command_source::{ConsensusNodeCommandSource, RebuildOptions};
    use tokio::sync::{mpsc, watch};
    use zksync_os_observability::ComponentStateReporter;
    use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
    use zksync_os_raft::{ConfirmedLeadership, ConsensusRole, LeadershipSignal};
    use zksync_os_sequencer::model::blocks::BlockCommand;

    let dir = tempfile::tempdir().unwrap();
    let original = chain(4);
    let mut replacement = original.clone();
    if changed_boundary {
        for number in 1..4 {
            let mut record = original[number].as_ref().clone();
            record.block_context.timestamp += 100;
            record.block_context.block_hashes = replacement[number - 1]
                .block_context
                .block_hashes
                .push(replacement[number - 1].hash());
            record.previous_block_timestamp = replacement[number - 1].block_context.timestamp;
            replacement[number] = Sealed::new_unchecked(record, block_hash(100 + number as u64));
        }
    }
    {
        let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
        for record in &original {
            storage.write(record.clone(), false).await.unwrap();
        }
        storage.write(replacement[1].clone(), true).await.unwrap();
    }
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let gate = zksync_os_backpressure::PipelineAdmissionGate::new();
    let (role_tx, role_rx) = watch::channel(ConfirmedLeadership {
        role: ConsensusRole::Replica,
        replay_watermark: 0,
    });
    let (pending_tx, pending_rx) = mpsc::unbounded_channel();
    let source = ConsensusNodeCommandSource {
        block_replay_storage: storage,
        starting_block: 1,
        rebuild_options: Some(RebuildOptions {
            from_block_number: 1,
            from_block_hash: original[1].hash(),
            blocks_to_empty: Default::default(),
            reset_timestamps,
        }),
        replays_to_execute: pending_rx,
        pipeline_gate: gate.subscribe(),
        leadership: LeadershipSignal::Watch(role_rx),
        produce_enabled: false,
    };
    let (_input_tx, input_rx) = mpsc::channel(1);
    let (output_tx, mut output_rx) = mpsc::channel(4);
    let (reporter, _) = ComponentStateReporter::new("syscoin_partial_raft_resume");
    let mut task = Box::pin(source.run(PeekableReceiver::new(input_rx), output_tx, reporter));
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(output_rx.try_recv().is_err());
    // SYSCOIN: Simulate Raft applying after pipeline construction and confirming leadership
    // while both suffix records are still held behind the asynchronous canonizer bridge.
    if leader {
        role_tx
            .send(ConfirmedLeadership {
                role: ConsensusRole::Leader,
                replay_watermark: 2,
            })
            .unwrap();
        assert!(futures::poll!(task.as_mut()).is_pending());
        assert!(
            output_rx.try_recv().is_err(),
            "leader proposed ahead of its replay watermark"
        );
    }
    pending_tx.send(replacement[2].as_ref().clone()).unwrap();
    if !changed_boundary && reset_timestamps {
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous rebuild resume"));
        assert!(output_rx.recv().await.is_none());
        return;
    }
    pending_tx.send(replacement[3].as_ref().clone()).unwrap();
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(
        matches!(output_rx.recv().await, Some(BlockCommand::Replay(record)) if *record == *replacement[1])
    );
    for expected in &replacement[2..4] {
        assert!(
            matches!(output_rx.recv().await, Some(BlockCommand::CanonizedRebuild(record)) if *record == **expected)
        );
    }
    assert!(output_rx.try_recv().is_err());
    drop(pending_tx);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn syscoin_partial_raft_rebuild_resumes_on_leader() {
    partial_raft_resume(true, true, true).await;
}

#[tokio::test]
async fn syscoin_partial_raft_rebuild_resumes_on_follower() {
    partial_raft_resume(false, true, true).await;
}

#[tokio::test]
async fn syscoin_noop_raft_prefix_resumes_without_changed_hash() {
    partial_raft_resume(true, false, false).await;
}

#[tokio::test]
async fn syscoin_ambiguous_raft_prefix_fails_closed() {
    partial_raft_resume(true, false, true).await;
}

// SYSCOIN: Poll the actual source deterministically, with two distinct channels representing
// the Raft-to-canonizer and canonizer-to-source bridge. No timing sleeps mask the empty-queue race.
async fn leader_watermark_orders_production(promote_after_start: bool) {
    use super::command_source::ConsensusNodeCommandSource;
    use tokio::sync::{mpsc, watch};
    use zksync_os_observability::ComponentStateReporter;
    use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};
    use zksync_os_raft::{ConfirmedLeadership, ConsensusRole, LeadershipSignal};
    use zksync_os_sequencer::model::blocks::BlockCommand;

    let dir = tempfile::tempdir().unwrap();
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    let records = chain(3);
    storage.write(records[0].clone(), false).await.unwrap();
    let gate = zksync_os_backpressure::PipelineAdmissionGate::new();
    let (role_tx, role_rx) = watch::channel(ConfirmedLeadership {
        role: if promote_after_start {
            ConsensusRole::Replica
        } else {
            ConsensusRole::Leader
        },
        replay_watermark: if promote_after_start { 0 } else { 2 },
    });
    let (raft_tx, mut raft_rx) = mpsc::unbounded_channel();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    let source = ConsensusNodeCommandSource {
        block_replay_storage: storage,
        starting_block: 1,
        rebuild_options: None,
        replays_to_execute: bridge_rx,
        pipeline_gate: gate.subscribe(),
        leadership: LeadershipSignal::Watch(role_rx),
        produce_enabled: true,
    };
    let (_input_tx, input_rx) = mpsc::channel(1);
    let (output_tx, mut output_rx) = mpsc::channel(4);
    let (reporter, _) = ComponentStateReporter::new("syscoin_leader_replay_watermark");
    let mut task = Box::pin(source.run(PeekableReceiver::new(input_rx), output_tx, reporter));
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(output_rx.try_recv().is_err());

    // Commits arrive after the source is created. Confirmation publishes the total number
    // forwarded by Raft, not the current length of either individual channel.
    for record in &records[1..] {
        raft_tx.send(record.as_ref().clone()).unwrap();
    }
    role_tx
        .send(ConfirmedLeadership {
            role: ConsensusRole::Leader,
            replay_watermark: 2,
        })
        .unwrap();
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(
        output_rx.try_recv().is_err(),
        "in-transit records must block Produce"
    );

    bridge_tx.send(raft_rx.try_recv().unwrap()).unwrap();
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(
        matches!(output_rx.try_recv(), Ok(BlockCommand::Replay(record)) if *record == *records[1])
    );
    assert!(
        output_rx.try_recv().is_err(),
        "partial watermark must still block Produce"
    );

    bridge_tx.send(raft_rx.try_recv().unwrap()).unwrap();
    assert!(futures::poll!(task.as_mut()).is_pending());
    assert!(
        matches!(output_rx.try_recv(), Ok(BlockCommand::Replay(record)) if *record == *records[2])
    );
    assert!(
        matches!(output_rx.try_recv(), Ok(BlockCommand::Produce(_))),
        "completed watermark must release production"
    );
    drop(output_rx);
    drop(bridge_tx);
    task.await.unwrap();
}

#[tokio::test]
async fn syscoin_initial_leader_waits_for_complete_bridge_watermark() {
    leader_watermark_orders_production(false).await;
}

#[tokio::test]
async fn syscoin_follower_promotion_waits_for_late_bridge_records() {
    leader_watermark_orders_production(true).await;
}

#[tokio::test]
async fn syscoin_complete_original_chain_is_linked() {
    let dir = tempfile::tempdir().unwrap();
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    for record in chain(4) {
        assert!(storage.write(record, false).await.unwrap());
    }
    assert!(replay_wal_is_linked_from(&storage, 1));
}

#[tokio::test]
async fn syscoin_interrupted_rebuild_must_not_look_complete() {
    let dir = tempfile::tempdir().unwrap();
    let original = chain(4);
    let replacement_hash = block_hash(101);
    {
        let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
        for record in &original {
            assert!(storage.write(record.clone(), false).await.unwrap());
        }
        let mut replacement = original[1].as_ref().clone();
        replacement.block_context.coinbase = Address::repeat_byte(0x99);
        replacement.block_output_hash = B256::repeat_byte(0x99);
        assert!(
            storage
                .write(Sealed::new_unchecked(replacement, replacement_hash), true)
                .await
                .unwrap()
        );
        // Stop here: blocks 2 and 3 have NOT been rebuilt. Close RocksDB to model restart.
    }
    let storage = BlockReplayStorage::new_without_genesis(dir.path(), 270);
    assert_eq!(storage.get_canonical_block_hash(1), Some(replacement_hash));
    assert_eq!(
        storage.get_canonical_block_hash(2),
        Some(original[2].hash())
    );
    let stale_tail = storage.get_replay_record(2).unwrap();
    assert_eq!(stale_tail.block_output_hash, original[2].block_output_hash);
    assert_ne!(
        original[2].block_context.block_hashes.0[255],
        U256::from_be_slice(replacement_hash.as_slice())
    );
    // This is the exact production startup guard, not a copy of its implementation.
    assert!(
        !replay_wal_is_linked_from(&storage, 1),
        "unfinished old-output tail was falsely classified as a completed rebuild"
    );
}

#[derive(Clone)]
struct EmptyState;

impl zksync_os_interface::traits::ReadStorage for EmptyState {
    fn read(&mut self, _key: B256) -> Option<B256> {
        None
    }
}

impl zksync_os_interface::traits::PreimageSource for EmptyState {
    fn get_preimage(&mut self, _hash: B256) -> Option<Vec<u8>> {
        None
    }
}

async fn execute_empty_record(
    context: BlockContext,
    previous_timestamp: u64,
    expected_output_hash: Option<B256>,
) -> (B256, ReplayRecord) {
    use zksync_os_interface::tracing::{NopTracer, NopValidator};
    use zksync_os_mempool::MarkingTxStream;
    use zksync_os_observability::ComponentStateReporter;
    use zksync_os_sequencer::execution::execute_block_in_vm::execute_block_in_vm;
    use zksync_os_sequencer::model::blocks::{InvalidTxPolicy, PreparedBlockCommand, SealPolicy};
    let (reporter, _) = ComponentStateReporter::new("archive_anchor_audit");
    let command = PreparedBlockCommand {
        block_context: context,
        seal_policy: SealPolicy::UntilExhausted {
            allowed_to_finish_early: false,
        },
        invalid_tx_policy: InvalidTxPolicy::Abort,
        tx_source: MarkingTxStream::unmarkable(futures::stream::empty()),
        metrics_label: "audit_replay",
        protocol_version: ProtocolSemanticVersion::canonical_genesis_version(),
        expected_block_output_hash: expected_output_hash,
        previous_block_timestamp: previous_timestamp,
        force_preimages: vec![],
        expect_sl_chain_id_tx_after_upgrade: false,
        starting_cursors: BlockStartCursors::default(),
        interop_roots_per_block: 10,
        strict_subpool_cleanup: false,
    };
    let (output, record, rejected, _) =
        execute_block_in_vm(command, EmptyState, &reporter, NopTracer, NopValidator)
            .await
            .unwrap_or_else(|dump| panic!("empty-block fixture failed: {}", dump.error));
    assert!(rejected.is_empty());
    (output.as_ref().header.hash(), record)
}

async fn archive_anchor_replay(substitute: bool) {
    use zksync_os_replay_archive::{ReplayArchiveSession, recover_replay_records_to_rocksdb};
    let input_dir = tempfile::tempdir().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let genesis_hash = zksync_os_genesis::genesis_header().hash();
    let mut genesis = chain(1).remove(0).into_inner();
    genesis.block_context.timestamp = 0;
    genesis.block_context.execution_version = 7;
    let context = BlockContext {
        chain_id: 270,
        block_number: 1,
        timestamp: 1_000,
        block_hashes: BlockHashes::default().push(genesis_hash),
        eip1559_basefee: U256::from(100),
        native_price: U256::from(100),
        pubdata_price: U256::from(100),
        gas_limit: 100_000_000,
        pubdata_limit: 1_000_000,
        execution_version: 7,
        blob_fee: U256::ONE,
        ..Default::default()
    };
    // Both records come from the actual patched VM and share the same correct parent.
    let (trusted_anchor, original_record) = execute_empty_record(context, 0, None).await;
    let (different_header, replacement_record) = execute_empty_record(
        BlockContext {
            coinbase: Address::repeat_byte(0x99),
            ..context
        },
        0,
        None,
    )
    .await;
    assert_ne!(trusted_anchor, different_header);
    assert_ne!(
        original_record.block_output_hash,
        replacement_record.block_output_hash
    );

    let archived_record = if substitute {
        &replacement_record
    } else {
        &original_record
    };
    let session = ReplayArchiveSession::new(42, "syscoin-recovery-test").unwrap();
    for (number, label_hash, record) in [
        (0, genesis_hash, &genesis),
        (1, trusted_anchor, archived_record),
    ] {
        let record_dir = input_dir
            .path()
            .join(number.to_string())
            .join(alloy::hex::encode_prefixed(label_hash.0));
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(
            record_dir.join(session.folder_name()),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }
    // The caller supplies the original, independently trusted canonical anchor.
    assert_eq!(
        recover_replay_records_to_rocksdb(input_dir.path(), db_dir.path(), 1, trusted_anchor)
            .await
            .unwrap(),
        2
    );
    let storage = BlockReplayStorage::new_without_genesis(db_dir.path(), 270);
    let recovered = storage.get_replay_record(1).unwrap();
    assert_eq!(storage.get_canonical_block_hash(1), Some(trusted_anchor));
    assert_eq!(&recovered, archived_record);
    // Reproduce the startup replay command's expected-hash check with the recovered record.
    let (reexecuted_header, reexecuted_record) = execute_empty_record(
        recovered.block_context,
        recovered.previous_block_timestamp,
        Some(recovered.block_output_hash),
    )
    .await;
    // SYSCOIN: The same persistence check used by BlockApplier must reject the alternate
    // actual VM header before applying any recovered state or publishing a repository block.
    let result = storage
        .write(
            Sealed::new_unchecked(reexecuted_record, reexecuted_header),
            false,
        )
        .await;
    if substitute {
        assert_eq!(reexecuted_header, different_header);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("canonical replay header mismatch")
        );
    } else {
        assert_eq!(reexecuted_header, trusted_anchor);
        assert!(
            !result.unwrap(),
            "identical canonical replay must remain idempotent"
        );
    }
    assert_eq!(storage.get_canonical_block_hash(1), Some(trusted_anchor));
}

#[tokio::test]
async fn syscoin_archive_anchor_rejects_substituted_terminal_header() {
    archive_anchor_replay(true).await;
}

#[tokio::test]
async fn syscoin_archive_anchor_accepts_matching_terminal_header() {
    archive_anchor_replay(false).await;
}
