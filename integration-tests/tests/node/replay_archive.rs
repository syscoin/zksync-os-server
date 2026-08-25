use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zksync_os_integration_tests::assert_traits::{DEFAULT_TIMEOUT, ReceiptAssert};
use zksync_os_integration_tests::provider::ZksyncTestingProvider;
use zksync_os_integration_tests::{CURRENT_TO_L1, TestEnvironment, test_multisetup};
use zksync_os_replay_archive::{
    ArchiveIdentity, DEFAULT_DECRYPT_CONCURRENCY, DEFAULT_DOWNLOAD_CONCURRENCY,
    FileSystemReplayArchiveReader, ReplayArchiveStorageReader, download_all_replay_archive_objects,
    read_age_x25519_identity, recover_replay_records_to_rocksdb_with_optional_decryption,
};
use zksync_os_server::config::{ReplayArchiveConfig, ReplayArchiveEncryptionConfig};
use zksync_os_storage::db::BlockReplayStorage;
use zksync_os_storage_api::ReadReplay;

const REPLAY_ARCHIVE_RECIPIENT: &str =
    "age1km7egrpfclsaf6tu4p3h2d8urcyp9s7cwcfzg2sezl95vmn0zgus8xhpk4";
const REPLAY_ARCHIVE_IDENTITY_FILE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/replay-archive.key");
const TRANSACTIONS_BEFORE_RECOVERY: usize = 3;

#[test_multisetup([CURRENT_TO_L1])]
#[test_runtime(flavor = "multi_thread")]
async fn encrypted_replay_archive_recovers_node_storage_end_to_end(
    env: TestEnvironment,
) -> anyhow::Result<()> {
    let mut config = env.default_config().await?;
    config.sequencer_config.block_time = Duration::from_millis(50);
    config.replay_archive_config = ReplayArchiveConfig::FileSystem {
        root_path: replay_archive_root(&config.general_config.rocks_db_path)?,
        encryption: ReplayArchiveEncryptionConfig::AgeX25519 {
            recipient: REPLAY_ARCHIVE_RECIPIENT.to_owned(),
        },
    };
    let tester = env.launch(config).await?;

    for _ in 0..TRANSACTIONS_BEFORE_RECOVERY {
        tester
            .l2_provider
            .send_transaction(
                TransactionRequest::default()
                    .with_to(Address::random())
                    .with_value(U256::from(1u64)),
            )
            .await?
            .expect_successful_receipt()
            .await?;
    }

    let archive_root = match &tester.config().replay_archive_config {
        ReplayArchiveConfig::FileSystem { root_path, .. } => root_path.clone(),
        _ => unreachable!("test enables replay archive"),
    };
    let chain_id = tester.l2_provider.get_chain_id().await?;
    let rocks_db_path = tester.config().general_config.rocks_db_path.clone();
    let stopped = tester.stop().await?;
    tokio::task::spawn_blocking(zksync_os_rocksdb::RocksDB::<()>::await_rocksdb_termination)
        .await
        .context("failed to join RocksDB shutdown wait")?;

    // Stopping a node can flush a partially accumulated batch and archive blocks beyond the last
    // RPC tip observed before shutdown. Recover from the writer-drained archive head so the
    // restored WAL cannot lag that newly committed batch.
    let archive_page = FileSystemReplayArchiveReader::new(archive_root.clone())
        .list_keys_page(None)
        .await?;
    let archive_head = archive_page
        .keys
        .iter()
        .max_by_key(|key| key.block_number)
        .context("replay archive should contain a canonical recovery anchor")?;
    let latest_block_number = archive_head.block_number;
    let latest_block_hash = archive_head.block_hash;

    tokio::fs::remove_dir_all(&rocks_db_path)
        .await
        .with_context(|| format!("failed to remove node storage {}", rocks_db_path.display()))?;

    recover_replay_storage_from_archive(
        &archive_root,
        &rocks_db_path,
        latest_block_number,
        latest_block_hash,
    )
    .await?;
    // SYSCOIN: Recovery writes a fresh RocksDB and drops it asynchronously. Ensure that handle is
    // fully gone before the in-process restart opens the same path, otherwise it can observe the
    // pre-recovery fixture handle and replay only its older tip.
    tokio::task::spawn_blocking(zksync_os_rocksdb::RocksDB::<()>::await_rocksdb_termination)
        .await
        .context("failed to join recovered RocksDB shutdown wait")?;
    // SYSCOIN: Verify recovery itself wrote the requested anchor before startup can transform any
    // state; this distinguishes archive recovery defects from later replay-pipeline behavior.
    let recovered_wal =
        BlockReplayStorage::new_without_genesis(&rocks_db_path.join("block_replay_wal"), chain_id);
    assert_eq!(
        recovered_wal.latest_record(),
        latest_block_number,
        "recovered WAL must end at the selected archive anchor"
    );
    assert_eq!(
        recovered_wal.get_canonical_block_hash(latest_block_number),
        Some(latest_block_hash),
        "recovered WAL anchor must preserve its canonical hash"
    );
    drop(recovered_wal);
    tokio::task::spawn_blocking(zksync_os_rocksdb::RocksDB::<()>::await_rocksdb_termination)
        .await
        .context("failed to join recovered WAL verification shutdown wait")?;

    let mut restarted = stopped.start().await?;
    // SYSCOIN: An archived tail can belong to a batch that shutdown had not sealed yet. Waiting
    // for that tail to finalize before producing another block deadlocks the test; repository
    // visibility plus the canonical hash is the correct recovery invariant at this point.
    let visibility_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if restarted.has_crashed() {
            let err = restarted
                .wait_for_fatal_error_with_timeout(Duration::from_secs(1))
                .await?;
            anyhow::bail!("node crashed while replaying the recovered archive: {err}");
        }
        let observed_rpc_tip = restarted.l2_provider.get_block_number().await?;
        if observed_rpc_tip >= latest_block_number {
            break;
        }
        if Instant::now() >= visibility_deadline {
            anyhow::bail!(
                "recovered replay archive head #{latest_block_number} was not visible through RPC (tip #{observed_rpc_tip})"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let recovered_head_hash = restarted
        .l2_provider
        .get_block_by_number(latest_block_number.into())
        .await?
        .context("recovered replay archive head should exist")?
        .header
        .hash;
    assert_eq!(
        recovered_head_hash, latest_block_hash,
        "recovered archive head must preserve its canonical hash"
    );
    let post_recovery_receipt = restarted
        .l2_provider
        .send_transaction(
            TransactionRequest::default()
                .with_to(Address::random())
                .with_value(U256::from(1u64)),
        )
        .await?
        .expect_successful_receipt()
        .await?;
    let post_recovery_block = post_recovery_receipt
        .block_number
        .context("post-recovery transaction receipt should have a block number")?;
    assert!(
        post_recovery_block > latest_block_number,
        "post-recovery block {post_recovery_block} should build on recovered tip {latest_block_number}"
    );
    restarted
        .l2_zk_provider
        .wait_finalized_with_timeout(post_recovery_block, DEFAULT_TIMEOUT)
        .await?;

    Ok(())
}

async fn recover_replay_storage_from_archive(
    archive_root: &Path,
    rocks_db_path: &Path,
    latest_block_number: u64,
    latest_block_hash: B256,
) -> anyhow::Result<()> {
    let recovery_root = archive_root
        .parent()
        .context("archive root should have a parent")?
        .join("replay_archive_recovery");
    let downloaded_root = recovery_root.join("downloaded");
    tokio::fs::create_dir_all(rocks_db_path)
        .await
        .with_context(|| {
            format!(
                "failed to create recovered node storage root {}",
                rocks_db_path.display()
            )
        })?;

    let reader = FileSystemReplayArchiveReader::new(archive_root.to_path_buf());
    let downloaded = download_all_replay_archive_objects(
        &reader,
        &downloaded_root,
        DEFAULT_DOWNLOAD_CONCURRENCY,
    )
    .await?;
    assert!(
        downloaded > 0,
        "replay archive should contain encrypted objects"
    );

    let identity = read_age_x25519_identity(Path::new(REPLAY_ARCHIVE_IDENTITY_FILE)).await?;
    let recovered = recover_replay_records_to_rocksdb_with_optional_decryption(
        &downloaded_root,
        &rocks_db_path.join("block_replay_wal"),
        latest_block_number,
        latest_block_hash,
        Some(ArchiveIdentity::X25519(identity)),
        DEFAULT_DECRYPT_CONCURRENCY,
    )
    .await?;
    assert_eq!(
        recovered,
        latest_block_number as usize + 1,
        "recovery should restore all canonical replay records from genesis through the anchor"
    );

    Ok(())
}

fn replay_archive_root(rocks_db_path: &Path) -> anyhow::Result<PathBuf> {
    Ok(rocks_db_path
        .parent()
        .context("rocks DB path should have a parent")?
        .join("replay_archive"))
}
