use crate::config::MAX_FRIS_PER_SNARK_HARD_CAP;
use alloy::primitives::{Address, B256, keccak256};
use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose};
use serde::de::{self, DeserializeSeed as _, Error as _, IgnoredAny, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::fs;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, mpsc};
use zk_ee::common_structs::MAX_NUMBER_OF_LOGS;
use zksync_os_batch_types::batcher_model::{
    BatchMetadata, BatchSignatureData, FriProof, L2_TO_L1_MESSENGER_ADDRESS, RealSnarkProof,
    SignedBatchEnvelope, SnarkProof,
};
use zksync_os_batch_types::{
    PendingBatchInfo, SYSCOIN_DA_MAX_BLOBS_PER_BATCH, SYSCOIN_DA_MAX_REFS_PER_BATCH,
};
use zksync_os_contract_interface::models::{L2Log, StoredBatchInfo};
use zksync_os_l1_sender::commands::prove::{ProofCommand, ZKSYNC_OS_V8_REAL_PROOF_BYTES};
use zksync_os_l1_watcher::CommittedBatchProvider;
use zksync_os_mini_merkle_tree::MiniMerkleTree;
use zksync_os_types::{
    L2_TO_L1_TREE_SIZE, L2ToL1Log, ProtocolSemanticVersion, ProvingVersion, PubdataMode,
};

// SYSCOIN: This version is independent of the prover HTTP schema. Any incompatible durable-record
// change must bump it and add an explicit migration; silently reinterpreting a wrapper is unsafe.
const JOURNAL_FORMAT_VERSION: u32 = 2;
const RETIRED_JOURNAL_FORMAT_VERSION: u32 = 1;
const JOURNAL_FILE_PREFIX: &str = "snark-";
const JOURNAL_FILE_SUFFIX: &str = ".json";
const JOURNAL_TEMP_PREFIX: &str = ".snark-journal-txn-";
const JOURNAL_TEMP_SUFFIX: &str = ".tmp";
const JOURNAL_QUARANTINE_DIR: &str = "unacknowledged-temp-quarantine";
const JOURNAL_PROCESS_LOCK_FILE: &str = ".snark-journal.lock";
// SYSCOIN: Real Airbender aggregation has a two-FRI minimum and the server hard-caps it at 100.
const MIN_JOURNALED_FRIS: usize = 2;
const MAX_JOURNALED_FRIS: usize = MAX_FRIS_PER_SNARK_HARD_CAP;
// SYSCOIN: The VM caps emitted L2-to-L1 logs at 16,384; each retained message must correspond to
// that bounded output set, and their aggregate bytes cannot exceed one 32-blob Bitcoin-DA batch.
const MAX_JOURNALED_LOGS_PER_BATCH: usize = MAX_NUMBER_OF_LOGS as usize;
const MAX_JOURNALED_MESSAGES_PER_BATCH: usize = MAX_NUMBER_OF_LOGS as usize;
const MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH: usize =
    SYSCOIN_DA_MAX_BLOBS_PER_BATCH * 2 * 1024 * 1024;
// SYSCOIN: Compact operator DA contains at most 32 blob hashes. A forwarded edge reference uses
// five ABI head words, one length word, and at least one hash; maximizing one-hash messages gives
// the conservative 32 * (6 ABI words + one hash) aggregate bound.
const MAX_JOURNALED_OPERATOR_DA_INPUT_BYTES: usize = SYSCOIN_DA_MAX_BLOBS_PER_BATCH * 32;
const MAX_JOURNALED_EDGE_DA_REFS_INPUT_BYTES: usize = SYSCOIN_DA_MAX_REFS_PER_BATCH * (6 * 32 + 32);
// SYSCOIN: V2 base64-compacts the only large binary field while preserving a single atomic JSON
// record. Bound both serialization and restart allocation at 256 MiB.
pub(crate) const MAX_JOURNAL_RECORD_BYTES: usize = 256 * 1024 * 1024;
static JOURNAL_TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

const fn padded_base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

const MAX_JOURNALED_MESSAGES_BASE64_BYTES: usize =
    padded_base64_len(MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH);
// SYSCOIN: One human-readable L2Log with maximum-width scalar values is 271 bytes. Array commas
// add one byte per item in this conservative symbolic bound.
const MAX_JOURNALED_LOG_JSON_BYTES: usize = 272;
// SYSCOIN: A u32 message length needs at most ten decimal digits plus one separator.
const MAX_JOURNALED_MESSAGE_LENGTH_JSON_BYTES: usize = 11;
// SYSCOIN: Every remaining V2 batch field is fixed-width or covered by the small operator/edge-DA
// bounds above. One MiB deliberately dominates their decimal JSON expansion and field framing.
const MAX_COMPACT_BATCH_FIXED_JSON_BYTES: usize = 1024 * 1024;
const MAX_COMPACT_JOURNALED_BATCH_JSON_BYTES: usize = MAX_COMPACT_BATCH_FIXED_JSON_BYTES
    + MAX_JOURNALED_MESSAGES_BASE64_BYTES
    + MAX_JOURNALED_MESSAGES_PER_BATCH * MAX_JOURNALED_MESSAGE_LENGTH_JSON_BYTES
    + MAX_JOURNALED_LOGS_PER_BATCH * MAX_JOURNALED_LOG_JSON_BYTES
    + MAX_JOURNALED_OPERATOR_DA_INPUT_BYTES * 4
    + MAX_JOURNALED_EDGE_DA_REFS_INPUT_BYTES * 4;
const MAX_COMPACT_RECORD_FIXED_JSON_BYTES: usize = 64 * 1024;
const MAX_COMPACT_MINIMUM_PAIR_JSON_BYTES: usize = MIN_JOURNALED_FRIS
    * MAX_COMPACT_JOURNALED_BATCH_JSON_BYTES
    + MAX_COMPACT_RECORD_FIXED_JSON_BYTES;

// SYSCOIN: Any two independently maximum-sized canonical batches fit the durable cap. This is the
// Airbender minimum, so cap splitting can never turn valid work into a deterministic fatal wedge.
const _: () = assert!(MAX_COMPACT_MINIMUM_PAIR_JSON_BYTES <= MAX_JOURNAL_RECORD_BYTES);

// SYSCOIN: Queue admission reserves the exact V2 base64 representation of the fixed V8 wrapper.
static WORST_CASE_SNARK_PROOF_JSON_BYTES: LazyLock<usize> = LazyLock::new(|| {
    serialized_json_len(&DurableRealSnarkProofV2 {
        proof_b64: general_purpose::STANDARD.encode(vec![u8::MAX; ZKSYNC_OS_V8_REAL_PROOF_BYTES]),
        proving_execution_version: ProvingVersion::V8 as u32,
    })
    .expect("fixed V8 proof JSON must serialize")
});

#[derive(Serialize)]
struct DurableSnarkBatchRef<'a> {
    batch: DurableBatchMetadataV2Ref<'a>,
}

// SYSCOIN: Precompute each marker-only batch's exact persisted JSON contribution before taking
// the queue lock. The aggregate picker can then split at the hard journal cap without serializing
// multi-megabyte message arrays while it owns global job-map state.
pub(crate) fn durable_snark_batch_json_bytes(batch: &BatchMetadata) -> anyhow::Result<usize> {
    serialized_json_len(&DurableSnarkBatchRef {
        batch: DurableBatchMetadataV2Ref::new(batch)?,
    })
}

// SYSCOIN: serde_json writes this record in declaration order without whitespace. Combine the
// precomputed element lengths, exact numeric widths, separators, and worst-case fixed proof to
// obtain a conservative-but-tight pre-lease bound for the final fsynced bytes.
pub(crate) fn durable_snark_record_json_upper_bound(
    batch_from: u64,
    batch_to: u64,
    batch_count: usize,
    batch_json_bytes: usize,
) -> Option<usize> {
    if batch_count == 0 {
        return None;
    }
    let fixed = r#"{"format_version":"#
        .len()
        .checked_add(decimal_digits(u64::from(JOURNAL_FORMAT_VERSION)))?
        .checked_add(r#","batch_from":"#.len())?;
    // The literals above and below deliberately mirror compact serde_json output. Keeping the
    // field fragments separate makes every checked addition auditable and avoids formatting.
    fixed
        .checked_add(decimal_digits(batch_from))?
        .checked_add(r#","batch_to":"#.len())?
        .checked_add(decimal_digits(batch_to))?
        .checked_add(r#","batches":["#.len())?
        .checked_add(batch_json_bytes)?
        .checked_add(batch_count.checked_sub(1)?)?
        .checked_add(r#"],"proof":"#.len())?
        .checked_add(*WORST_CASE_SNARK_PROOF_JSON_BYTES)?
        .checked_add(1)
}

fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

struct JsonLengthWriter(usize);

impl std::io::Write for JsonLengthWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "durable SNARK JSON length overflow",
            )
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_len(value: &impl Serialize) -> anyhow::Result<usize> {
    let mut writer = JsonLengthWriter(0);
    serde_json::to_writer(&mut writer, value).context("measure durable SNARK journal JSON")?;
    Ok(writer.0)
}

#[derive(Serialize)]
struct DurableBatchMetadataV2Ref<'a> {
    previous_stored_batch_info: &'a StoredBatchInfo,
    #[serde(rename = "commit_batch_info")]
    batch_info: &'a PendingBatchInfo,
    chain_address: Address,
    first_block_number: u64,
    last_block_number: u64,
    last_block_hash: Option<B256>,
    pubdata_mode: &'a PubdataMode,
    tx_count: usize,
    computational_native_used: Option<u64>,
    logs: &'a [L2Log],
    message_lengths: Vec<u32>,
    messages_b64: String,
    multichain_root: B256,
    set_sl_chain_id_migration_number: Option<u64>,
}

impl<'a> DurableBatchMetadataV2Ref<'a> {
    fn new(batch: &'a BatchMetadata) -> anyhow::Result<Self> {
        anyhow::ensure!(
            batch.logs.len() <= MAX_JOURNALED_LOGS_PER_BATCH,
            "batch logs exceed durable SNARK journal limit"
        );
        let (message_lengths, messages_b64) = compact_messages(&batch.messages)?;
        Ok(Self {
            previous_stored_batch_info: &batch.previous_stored_batch_info,
            batch_info: &batch.batch_info,
            chain_address: batch.chain_address,
            first_block_number: batch.first_block_number,
            last_block_number: batch.last_block_number,
            last_block_hash: batch.last_block_hash,
            pubdata_mode: &batch.pubdata_mode,
            tx_count: batch.tx_count,
            computational_native_used: batch.computational_native_used,
            logs: &batch.logs,
            message_lengths,
            messages_b64,
            multichain_root: batch.multichain_root,
            set_sl_chain_id_migration_number: batch.set_sl_chain_id_migration_number,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableBatchMetadataV2 {
    previous_stored_batch_info: StoredBatchInfo,
    #[serde(rename = "commit_batch_info")]
    batch_info: PendingBatchInfo,
    chain_address: Address,
    first_block_number: u64,
    last_block_number: u64,
    last_block_hash: Option<B256>,
    pubdata_mode: PubdataMode,
    tx_count: usize,
    computational_native_used: Option<u64>,
    logs: Vec<L2Log>,
    message_lengths: Vec<u32>,
    messages_b64: String,
    multichain_root: B256,
    set_sl_chain_id_migration_number: Option<u64>,
}

impl DurableBatchMetadataV2 {
    fn into_batch_metadata(self) -> anyhow::Result<BatchMetadata> {
        anyhow::ensure!(
            self.logs.len() <= MAX_JOURNALED_LOGS_PER_BATCH,
            "journal batch logs exceed {MAX_JOURNALED_LOGS_PER_BATCH} entries"
        );
        let messages = expand_messages(self.message_lengths, &self.messages_b64)?;
        Ok(BatchMetadata {
            previous_stored_batch_info: self.previous_stored_batch_info,
            batch_info: self.batch_info,
            chain_address: self.chain_address,
            first_block_number: self.first_block_number,
            last_block_number: self.last_block_number,
            last_block_hash: self.last_block_hash,
            pubdata_mode: self.pubdata_mode,
            tx_count: self.tx_count,
            computational_native_used: self.computational_native_used,
            logs: self.logs,
            messages,
            multichain_root: self.multichain_root,
            set_sl_chain_id_migration_number: self.set_sl_chain_id_migration_number,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSnarkBatchV2 {
    batch: DurableBatchMetadataV2,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableRealSnarkProofV2 {
    proof_b64: String,
    proving_execution_version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSnarkRecordV2 {
    format_version: u32,
    batch_from: u64,
    batch_to: u64,
    batches: Vec<DurableSnarkBatchV2>,
    proof: DurableRealSnarkProofV2,
}

#[derive(Deserialize)]
struct JournalVersionProbe {
    format_version: u32,
}

fn compact_messages(messages: &[Vec<u8>]) -> anyhow::Result<(Vec<u32>, String)> {
    anyhow::ensure!(
        messages.len() <= MAX_JOURNALED_MESSAGES_PER_BATCH,
        "batch messages exceed durable SNARK journal limit"
    );
    let mut total_bytes = 0usize;
    let mut lengths = Vec::with_capacity(messages.len());
    for message in messages {
        total_bytes = total_bytes
            .checked_add(message.len())
            .context("batch message byte count overflow")?;
        anyhow::ensure!(
            total_bytes <= MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH,
            "batch message bytes exceed durable SNARK journal limit"
        );
        lengths.push(u32::try_from(message.len()).context("batch message exceeds u32 length")?);
    }
    let mut concatenated = Vec::new();
    concatenated
        .try_reserve_exact(total_bytes)
        .context("reserve compact journal message buffer")?;
    for message in messages {
        concatenated.extend_from_slice(message);
    }
    Ok((lengths, general_purpose::STANDARD.encode(concatenated)))
}

fn expand_messages(lengths: Vec<u32>, encoded: &str) -> anyhow::Result<Vec<Vec<u8>>> {
    anyhow::ensure!(
        lengths.len() <= MAX_JOURNALED_MESSAGES_PER_BATCH,
        "journal messages exceed {MAX_JOURNALED_MESSAGES_PER_BATCH} entries"
    );
    let total_bytes = lengths
        .iter()
        .try_fold(0usize, |total, length| total.checked_add(*length as usize));
    let total_bytes = total_bytes.context("journal message byte count overflow")?;
    anyhow::ensure!(
        total_bytes <= MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH,
        "journal message bytes exceed {MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH} per batch"
    );
    anyhow::ensure!(
        encoded.len() == padded_base64_len(total_bytes),
        "journal compact-message base64 length does not match declared message lengths"
    );
    let decoded = general_purpose::STANDARD
        .decode(encoded)
        .context("decode journal compact-message base64")?;
    anyhow::ensure!(
        decoded.len() == total_bytes,
        "journal compact-message bytes do not match declared message lengths"
    );

    let mut messages = Vec::with_capacity(lengths.len());
    let mut offset = 0usize;
    for length in lengths {
        let end = offset
            .checked_add(length as usize)
            .context("journal message offset overflow")?;
        messages.push(decoded[offset..end].to_vec());
        offset = end;
    }
    anyhow::ensure!(
        offset == decoded.len(),
        "journal compact-message trailing bytes"
    );
    Ok(messages)
}

/// SYSCOIN: One self-contained, versioned real-wrapper record. FRI bytes are deliberately replaced
/// by `AlreadySubmittedToL1`. Commit signatures authorize the earlier commit stage only; prove and
/// execute never consume them, and recovery rebinds every batch (including the first predecessor)
/// to `CommittedBatchProvider`. Persisting those stale bytes would create an unvalidated second
/// source of authorization, so the journal accepts only the typed `AlreadyCommitted` marker.
struct DurableSnarkRecord {
    format_version: u32,
    batch_from: u64,
    batch_to: u64,
    batches: Vec<SignedBatchEnvelope<FriProof>>,
    proof: SnarkProof,
}

/// SYSCOIN: A validated journal record ready to enter the L1 proof pipeline.
pub(super) struct JournaledProof {
    key: String,
    batches: Vec<SignedBatchEnvelope<FriProof>>,
    proof: SnarkProof,
}

/// SYSCOIN: A startup record already reflected by latest SL state but not yet by the historical
/// state block with the configured confirmation depth. It remains durable and is never replayed
/// against the still-covered range while startup waits for confirmation or detects rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingStartupConfirmation {
    key: String,
    batch_from: u64,
    batch_to: u64,
}

impl PendingStartupConfirmation {
    pub(super) fn batch_range(&self) -> (u64, u64) {
        (self.batch_from, self.batch_to)
    }
}

pub(super) struct RecoveredSnarkJournal {
    pub(super) replay: Vec<JournaledProof>,
    pub(super) pending_confirmation: Vec<PendingStartupConfirmation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupJournalDisposition {
    Retired,
    PendingConfirmation,
    Replay,
}

trait StartupRecordValidator {
    async fn validate(&self, record: &DurableSnarkRecord) -> anyhow::Result<()>;
}

struct CommittedStartupRecordValidator<'a> {
    chain_id: u64,
    chain_address: Address,
    committed_batches: &'a CommittedBatchProvider,
}

impl StartupRecordValidator for CommittedStartupRecordValidator<'_> {
    async fn validate(&self, record: &DurableSnarkRecord) -> anyhow::Result<()> {
        validate_record_against_committed(
            record,
            self.chain_id,
            self.chain_address,
            self.committed_batches,
        )
        .await
    }
}

impl JournaledProof {
    pub(super) fn batch_range(&self) -> (u64, u64) {
        (
            self.batches
                .first()
                .expect("validated journal is non-empty")
                .batch_number(),
            self.batches
                .last()
                .expect("validated journal is non-empty")
                .batch_number(),
        )
    }

    /// SYSCOIN: Recovered wrappers are preflighted in place before ownership moves into the L1
    /// pipeline; exposing immutable inputs avoids cloning proof bytes or bypassing journal state.
    pub(super) fn preflight_inputs(&self) -> (&[SignedBatchEnvelope<FriProof>], &SnarkProof) {
        (&self.batches, &self.proof)
    }

    pub(super) fn into_command(
        self,
        confirmation_sender: mpsc::UnboundedSender<String>,
    ) -> ProofCommand {
        ProofCommand::new_durable(self.batches, self.proof, self.key, confirmation_sender)
    }
}

// SYSCOIN: Proof bytes are intentionally redacted from diagnostics; only non-secret routing and
// allocation metadata may appear in panic or error output.
impl fmt::Debug for JournaledProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournaledProof")
            .field("key", &self.key)
            .field("batch_range", &self.batch_range())
            .field("batch_count", &self.batches.len())
            .field("proof_bytes", &self.proof.proof().map(<[u8]>::len))
            .field(
                "proving_execution_version",
                &self.proof.proving_execution_version(),
            )
            .finish()
    }
}

#[derive(Debug, Clone)]
struct JournalIndexEntry {
    key: String,
    batch_to: u64,
}

/// SYSCOIN: Unconfirmed records are never capacity-evicted. Filesystem exhaustion is reported to
/// the HTTP manager as retryable while its exact lease and the prover's identical proof remain live.
#[derive(Clone, Debug)]
pub(crate) struct SnarkProofJournal {
    inner: Arc<SnarkProofJournalInner>,
    // SYSCOIN: Keep senders outside the reaper's shared inner state so its own handle can be
    // dropped before receiving; shutdown then closes once all actual producers are gone.
    confirmation_sender: mpsc::UnboundedSender<String>,
}

#[derive(Debug)]
struct SnarkProofJournalInner {
    directory: PathBuf,
    records: Mutex<BTreeMap<u64, JournalIndexEntry>>,
    #[cfg(test)]
    fail_next_persist: std::sync::atomic::AtomicBool,
    // SYSCOIN: Holding this open file description for the Arc lifetime exclusively owns journal
    // publication and cleanup for this proof-storage directory.
    _process_lock: JournalProcessLock,
}

// SYSCOIN: Never expose OS handle details in journal diagnostics.
struct JournalProcessLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

impl fmt::Debug for JournalProcessLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JournalProcessLock(<held>)")
    }
}

impl SnarkProofJournal {
    /// SYSCOIN: Open and structurally validate every durable wrapper before the prover API starts.
    /// Malformed or overlapping final records fail startup closed; only unacknowledged transaction
    /// temps are quarantined because the API cannot have returned 204 before final publication.
    pub(crate) async fn open(
        proof_storage_path: &Path,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<String>)> {
        let directory = proof_storage_path.join("snark_journal");
        fs::create_dir_all(&directory)
            .await
            .with_context(|| format!("create SNARK journal directory {}", directory.display()))?;
        set_owner_only_directory(&directory).await?;
        // SYSCOIN: Acquire before inspecting or mutating temps/index state; two server processes
        // must never publish from divergent in-memory indexes into one durable namespace.
        let process_lock = acquire_process_lock(&directory).await?;
        quarantine_transaction_temps(&directory).await?;

        let mut records = BTreeMap::new();
        let mut entries = fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_dir() && entry.file_name() == JOURNAL_QUARANTINE_DIR {
                continue;
            }
            if file_type.is_file() && entry.file_name() == JOURNAL_PROCESS_LOCK_FILE {
                continue;
            }
            anyhow::ensure!(
                file_type.is_file(),
                "unexpected non-file in SNARK journal: {}",
                entry.path().display()
            );
            let key = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF8 filename in SNARK journal"))?;
            let filename_range = parse_journal_key(&key)
                .with_context(|| format!("unexpected file in SNARK journal: {key}"))?;
            // SYSCOIN: Accept only the one canonical fixed-width key for a range; alternate
            // numeric spellings must not create duplicate names for the same durable owner.
            anyhow::ensure!(
                key == journal_key(filename_range.0, filename_range.1),
                "non-canonical filename in SNARK journal: {key}"
            );
            let record = load_record(&entry.path()).await?;
            validate_record_structure(&record)
                .with_context(|| format!("validate durable SNARK journal {key}"))?;
            anyhow::ensure!(
                filename_range == (record.batch_from, record.batch_to),
                "SNARK journal filename/range mismatch for {key}"
            );
            ensure_no_overlap(&records, record.batch_from, record.batch_to)?;
            records.insert(
                record.batch_from,
                JournalIndexEntry {
                    key,
                    batch_to: record.batch_to,
                },
            );
        }

        let (confirmation_sender, confirmation_receiver) = mpsc::unbounded_channel();
        Ok((
            Self {
                inner: Arc::new(SnarkProofJournalInner {
                    directory,
                    records: Mutex::new(records),
                    #[cfg(test)]
                    fail_next_persist: std::sync::atomic::AtomicBool::new(false),
                    _process_lock: process_lock,
                }),
                confirmation_sender,
            },
            confirmation_receiver,
        ))
    }

    pub(super) fn confirmation_sender(&self) -> mpsc::UnboundedSender<String> {
        self.confirmation_sender.clone()
    }

    pub(super) async fn has_records(&self) -> bool {
        !self.inner.records.lock().await.is_empty()
    }

    #[cfg(test)]
    pub(super) async fn record_count(&self) -> usize {
        self.inner.records.lock().await.len()
    }

    #[cfg(test)]
    pub(super) fn fail_next_persist_for_test(&self) {
        self.inner.fail_next_persist.store(true, Ordering::SeqCst);
    }

    /// SYSCOIN: Publish the self-contained record before exact leased jobs are removed or a
    /// terminal acceptance reaches the prover. Repeating identical bytes is idempotent; any
    /// overlapping-but-different record is an invariant failure and remains untouched.
    pub(super) async fn persist(
        &self,
        batches: Vec<SignedBatchEnvelope<FriProof>>,
        proof: SnarkProof,
    ) -> anyhow::Result<JournaledProof> {
        let batch_from = batches
            .first()
            .context("cannot journal an empty SNARK aggregate")?
            .batch_number();
        let batch_to = batches
            .last()
            .context("cannot journal an empty SNARK aggregate")?
            .batch_number();
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from,
            batch_to,
            batches,
            proof,
        };
        validate_record_structure(&record)?;
        let serialized = serialize_record(&record)?;
        #[cfg(test)]
        if self.inner.fail_next_persist.swap(false, Ordering::SeqCst) {
            anyhow::bail!("injected durable SNARK journal I/O failure");
        }
        let key = journal_key(batch_from, batch_to);

        let mut records = self.inner.records.lock().await;
        if let Some(existing) = records.get(&batch_from) {
            anyhow::ensure!(
                existing.batch_to == batch_to && existing.key == key,
                "conflicting durable SNARK journal starts at batch {batch_from}"
            );
            let existing_bytes =
                read_bounded_journal_file(&self.inner.directory.join(&key)).await?;
            anyhow::ensure!(
                existing_bytes == serialized,
                "conflicting durable SNARK journal content for range {batch_from}-{batch_to}"
            );
            return Ok(record.into_journaled(key));
        }
        ensure_no_overlap(&records, batch_from, batch_to)?;

        // SYSCOIN: A prior attempt may have completed atomic publication but failed the final
        // directory fsync/temporary cleanup. Discover identical final bytes without requiring a
        // process restart; never overwrite different content.
        let final_path = self.inner.directory.join(&key);
        if fs::try_exists(&final_path).await? {
            let existing_bytes = read_bounded_journal_file(&final_path).await?;
            anyhow::ensure!(
                existing_bytes == serialized,
                "conflicting unindexed durable SNARK journal content for range {batch_from}-{batch_to}"
            );
            sync_directory(&self.inner.directory).await?;
            records.insert(
                batch_from,
                JournalIndexEntry {
                    key: key.clone(),
                    batch_to,
                },
            );
            return Ok(record.into_journaled(key));
        }

        durable_publish_new(&self.inner.directory, &key, &serialized).await?;
        records.insert(
            batch_from,
            JournalIndexEntry {
                key: key.clone(),
                batch_to,
            },
        );
        Ok(record.into_journaled(key))
    }

    /// SYSCOIN: Load, canonical-state validate, and order all replayable wrappers before any FRI
    /// queue rehydration. Startup L1 state retires only records whose entire range is proved.
    // SYSCOIN: Both latest and confirmation-safe frontiers plus canonical topology are independent
    // recovery authorities; keeping them explicit prevents an unsafe inferred settlement policy.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn recover(
        &self,
        last_proved_batch: u64,
        confirmation_safe_proved_batch: u64,
        last_committed_batch: u64,
        chain_id: u64,
        chain_address: Address,
        real_proving: bool,
        committed_batches: &CommittedBatchProvider,
    ) -> anyhow::Result<RecoveredSnarkJournal> {
        let validator = CommittedStartupRecordValidator {
            chain_id,
            chain_address,
            committed_batches,
        };
        self.recover_with_validator(
            last_proved_batch,
            confirmation_safe_proved_batch,
            last_committed_batch,
            real_proving,
            &validator,
        )
        .await
    }

    // SYSCOIN: Keep current-topology validation behind the Replay disposition. The generic seam is
    // also the regression fixture proving a historical-proxy Pending record can never reach it.
    async fn recover_with_validator<V: StartupRecordValidator>(
        &self,
        last_proved_batch: u64,
        confirmation_safe_proved_batch: u64,
        last_committed_batch: u64,
        real_proving: bool,
        validator: &V,
    ) -> anyhow::Result<RecoveredSnarkJournal> {
        anyhow::ensure!(
            confirmation_safe_proved_batch <= last_proved_batch,
            "confirmation-safe proved frontier {confirmation_safe_proved_batch} is ahead of latest proved frontier {last_proved_batch}"
        );
        let entries: Vec<_> = self.inner.records.lock().await.values().cloned().collect();
        anyhow::ensure!(
            real_proving || entries.is_empty(),
            "real durable SNARK journals exist while the node is configured for fake proving"
        );
        let mut replay = Vec::with_capacity(entries.len());
        let mut pending_confirmation = Vec::new();

        for entry in entries {
            let path = self.inner.directory.join(&entry.key);
            let record = load_record(&path).await?;
            validate_record_structure(&record)
                .with_context(|| format!("validate durable SNARK journal {}", entry.key))?;
            // SYSCOIN: Rebind the second read to the locked startup index before acting on L1
            // state; a mismatched filename/range can neither be retired nor replayed.
            anyhow::ensure!(
                parse_journal_key(&entry.key) == Some((record.batch_from, record.batch_to))
                    && entry.batch_to == record.batch_to,
                "durable SNARK journal index/range mismatch for {}",
                entry.key
            );
            anyhow::ensure!(
                record.batch_to <= last_committed_batch,
                "durable SNARK journal {} extends beyond startup committed batch {}",
                entry.key,
                last_committed_batch
            );
            match self
                .classify_startup_record(
                    &entry,
                    &record,
                    last_proved_batch,
                    confirmation_safe_proved_batch,
                )
                .await?
            {
                StartupJournalDisposition::Retired => continue,
                StartupJournalDisposition::PendingConfirmation => {
                    // SYSCOIN: Latest canonical proved state, not the current settlement proxy,
                    // owns this covered record. A wrapper may span a historical Gateway interval
                    // and is never replayed while pending; validating it against today's proxy
                    // would make an otherwise safe migration fail startup. Any latest-frontier
                    // movement forces full rediscovery before this classification can be used.
                    pending_confirmation.push(PendingStartupConfirmation {
                        key: entry.key,
                        batch_from: record.batch_from,
                        batch_to: record.batch_to,
                    });
                    continue;
                }
                StartupJournalDisposition::Replay => {}
            }
            validator.validate(&record).await.with_context(|| {
                format!(
                    "canonical validation for durable SNARK journal {}",
                    entry.key
                )
            })?;
            replay.push(record.into_journaled(entry.key));
        }
        replay.sort_by_key(JournaledProof::batch_range);
        validate_replay_ranges(last_proved_batch, &replay)?;
        pending_confirmation.sort_by_key(PendingStartupConfirmation::batch_range);
        Ok(RecoveredSnarkJournal {
            replay,
            pending_confirmation,
        })
    }

    async fn classify_startup_record(
        &self,
        entry: &JournalIndexEntry,
        record: &DurableSnarkRecord,
        last_proved_batch: u64,
        confirmation_safe_proved_batch: u64,
    ) -> anyhow::Result<StartupJournalDisposition> {
        anyhow::ensure!(
            confirmation_safe_proved_batch <= last_proved_batch,
            "confirmation-safe proved frontier is ahead of latest proved frontier"
        );
        if record.batch_to <= confirmation_safe_proved_batch {
            tracing::info!(
                journal_key = entry.key,
                batch_from = record.batch_from,
                batch_to = record.batch_to,
                last_proved_batch,
                confirmation_safe_proved_batch,
                "retiring confirmation-safe startup-covered durable SNARK journal"
            );
            self.remove_confirmed(&entry.key).await?;
            return Ok(StartupJournalDisposition::Retired);
        }
        if record.batch_to <= last_proved_batch {
            tracing::info!(
                journal_key = entry.key,
                batch_from = record.batch_from,
                batch_to = record.batch_to,
                last_proved_batch,
                confirmation_safe_proved_batch,
                "retaining latest-covered durable SNARK journal until confirmation depth"
            );
            return Ok(StartupJournalDisposition::PendingConfirmation);
        }
        // SYSCOIN: Settlement is aggregate-atomic. A frontier strictly inside a journal range
        // cannot prove which bytes/contracts advanced it, so preserve the file and fail closed.
        anyhow::ensure!(
            record.batch_from > last_proved_batch,
            "startup proved frontier {last_proved_batch} partially covers durable SNARK journal {} range {}-{}",
            entry.key,
            record.batch_from,
            record.batch_to
        );
        Ok(StartupJournalDisposition::Replay)
    }

    /// SYSCOIN: Startup may retire a latest-covered wrapper only after a fresh canonical snapshot
    /// advances the safe proved frontier across its whole aggregate.
    pub(super) async fn retire_startup_confirmed(
        &self,
        pending: &[PendingStartupConfirmation],
        confirmation_safe_proved_batch: u64,
    ) -> anyhow::Result<()> {
        for record in pending {
            anyhow::ensure!(
                record.batch_to <= confirmation_safe_proved_batch,
                "confirmation-safe proved frontier {confirmation_safe_proved_batch} does not cover pending durable SNARK journal {} range {}-{}",
                record.key,
                record.batch_from,
                record.batch_to
            );
            self.remove_confirmed(&record.key).await?;
        }
        Ok(())
    }

    /// SYSCOIN: Confirmation cleanup is idempotent. The in-memory index advances only after the
    /// unlink (or already-absent state) and parent-directory fsync both succeed.
    async fn remove_confirmed(&self, key: &str) -> anyhow::Result<()> {
        Self::remove_confirmed_from(&self.inner, key).await
    }

    async fn remove_confirmed_from(
        inner: &SnarkProofJournalInner,
        key: &str,
    ) -> anyhow::Result<()> {
        let Some((batch_from, batch_to)) = parse_journal_key(key) else {
            anyhow::bail!("invalid SNARK journal confirmation key {key}");
        };
        let mut records = inner.records.lock().await;
        let Some(indexed) = records.get(&batch_from) else {
            sync_directory(&inner.directory).await?;
            return Ok(());
        };
        anyhow::ensure!(
            indexed.key == key && indexed.batch_to == batch_to,
            "SNARK journal confirmation conflicts with indexed range"
        );
        let path = inner.directory.join(key);
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        sync_directory(&inner.directory).await?;
        records.remove(&batch_from);
        Ok(())
    }

    /// SYSCOIN: Failed local cleanup retains (or may conservatively resurrect) a confirmed record;
    /// startup `last_proved` pruning makes that harmless and prevents proof loss.
    pub(super) async fn run_reaper(
        self,
        mut confirmations: mpsc::UnboundedReceiver<String>,
    ) -> anyhow::Result<()> {
        // SYSCOIN: The receiver task must not keep its own channel alive after all proof-command
        // producers shut down. The shared inner intentionally contains no sender.
        let inner = self.inner.clone();
        drop(self.confirmation_sender);
        while let Some(key) = confirmations.recv().await {
            if let Err(error) = Self::remove_confirmed_with_retry(&inner, &key).await {
                tracing::error!(
                    journal_key = key,
                    ?error,
                    "failed to retire confirmed durable SNARK journal; retaining for restart cleanup"
                );
            }
        }
        anyhow::bail!("durable SNARK journal confirmation channel closed")
    }

    // SYSCOIN: Transient unlink/fsync failures must not orphan a confirmed proof until restart.
    // Retry with bounded exponential backoff, then retain the indexed file for startup pruning.
    async fn remove_confirmed_with_retry(
        inner: &SnarkProofJournalInner,
        key: &str,
    ) -> anyhow::Result<()> {
        const ATTEMPTS: usize = 5;
        let mut backoff = Duration::from_millis(100);
        let mut final_error = None;
        for attempt in 1..=ATTEMPTS {
            match Self::remove_confirmed_from(inner, key).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    final_error = Some(error);
                    if attempt < ATTEMPTS {
                        tracing::warn!(
                            journal_key = key,
                            attempt,
                            max_attempts = ATTEMPTS,
                            "retrying confirmed durable SNARK journal cleanup"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = backoff.saturating_mul(2);
                    }
                }
            }
        }
        Err(final_error.expect("at least one journal cleanup attempt"))
    }
}

fn validate_replay_ranges(last_proved_batch: u64, replay: &[JournaledProof]) -> anyhow::Result<()> {
    let mut previous_to = None;
    for journaled in replay {
        let (batch_from, batch_to) = journaled.batch_range();
        // SYSCOIN: Gaps are valid after an out-of-order SNARK lease completes: the pipeline
        // rehydrates each missing range from durable FRI storage. Covered/partial ranges are not.
        anyhow::ensure!(
            batch_from > last_proved_batch,
            "durable SNARK replay range {batch_from}-{batch_to} intersects proved frontier {last_proved_batch}"
        );
        anyhow::ensure!(
            batch_from <= batch_to,
            "durable SNARK replay range is inverted: {batch_from}-{batch_to}"
        );
        if let Some(previous_to) = previous_to {
            anyhow::ensure!(
                previous_to < batch_from,
                "durable SNARK replay ranges overlap at {batch_from}-{batch_to} after batch {previous_to}"
            );
        }
        previous_to = Some(batch_to);
    }
    Ok(())
}

impl DurableSnarkRecord {
    fn into_journaled(self, key: String) -> JournaledProof {
        JournaledProof {
            key,
            batches: self.batches,
            proof: self.proof,
        }
    }
}

impl DurableBatchMetadataV2 {
    fn from_batch_metadata(batch: &BatchMetadata) -> anyhow::Result<Self> {
        anyhow::ensure!(
            batch.logs.len() <= MAX_JOURNALED_LOGS_PER_BATCH,
            "batch logs exceed durable SNARK journal limit"
        );
        let (message_lengths, messages_b64) = compact_messages(&batch.messages)?;
        Ok(Self {
            previous_stored_batch_info: batch.previous_stored_batch_info.clone(),
            batch_info: batch.batch_info.clone(),
            chain_address: batch.chain_address,
            first_block_number: batch.first_block_number,
            last_block_number: batch.last_block_number,
            last_block_hash: batch.last_block_hash,
            pubdata_mode: batch.pubdata_mode,
            tx_count: batch.tx_count,
            computational_native_used: batch.computational_native_used,
            logs: batch.logs.clone(),
            message_lengths,
            messages_b64,
            multichain_root: batch.multichain_root,
            set_sl_chain_id_migration_number: batch.set_sl_chain_id_migration_number,
        })
    }
}

impl DurableSnarkRecordV2 {
    fn from_record(record: &DurableSnarkRecord) -> anyhow::Result<Self> {
        let SnarkProof::Real(real) = &record.proof else {
            anyhow::bail!("fake SNARKs cannot enter the durable journal");
        };
        let batches = record
            .batches
            .iter()
            .map(|batch| {
                Ok(DurableSnarkBatchV2 {
                    batch: DurableBatchMetadataV2::from_batch_metadata(&batch.batch)?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: record.batch_from,
            batch_to: record.batch_to,
            batches,
            proof: DurableRealSnarkProofV2 {
                proof_b64: general_purpose::STANDARD.encode(real.proof()),
                proving_execution_version: real.proving_execution_version,
            },
        })
    }

    fn into_record(self) -> anyhow::Result<DurableSnarkRecord> {
        anyhow::ensure!(
            self.format_version == JOURNAL_FORMAT_VERSION,
            "unsupported durable SNARK journal version {}",
            self.format_version
        );
        anyhow::ensure!(
            self.proof.proof_b64.len() == padded_base64_len(ZKSYNC_OS_V8_REAL_PROOF_BYTES),
            "journaled V8 SNARK proof base64 has invalid length"
        );
        let proof = general_purpose::STANDARD
            .decode(&self.proof.proof_b64)
            .context("decode journaled V8 SNARK proof base64")?;
        anyhow::ensure!(
            proof.len() == ZKSYNC_OS_V8_REAL_PROOF_BYTES,
            "journaled V8 SNARK proof must be exactly {ZKSYNC_OS_V8_REAL_PROOF_BYTES} bytes"
        );
        let batches = self
            .batches
            .into_iter()
            .map(|batch| {
                Ok(SignedBatchEnvelope {
                    batch: batch.batch.into_batch_metadata()?,
                    data: FriProof::AlreadySubmittedToL1,
                    signature_data: BatchSignatureData::AlreadyCommitted,
                    latency_tracker: Default::default(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(DurableSnarkRecord {
            format_version: self.format_version,
            batch_from: self.batch_from,
            batch_to: self.batch_to,
            batches,
            proof: SnarkProof::Real(RealSnarkProof {
                proof,
                proving_execution_version: self.proof.proving_execution_version,
            }),
        })
    }
}

fn journal_key(batch_from: u64, batch_to: u64) -> String {
    format!("{JOURNAL_FILE_PREFIX}{batch_from:020}-{batch_to:020}{JOURNAL_FILE_SUFFIX}")
}

fn parse_journal_key(key: &str) -> Option<(u64, u64)> {
    let range = key
        .strip_prefix(JOURNAL_FILE_PREFIX)?
        .strip_suffix(JOURNAL_FILE_SUFFIX)?;
    let (from, to) = range.split_once('-')?;
    if from.len() != 20 || to.len() != 20 {
        return None;
    }
    Some((from.parse().ok()?, to.parse().ok()?))
}

fn ensure_no_overlap(
    records: &BTreeMap<u64, JournalIndexEntry>,
    batch_from: u64,
    batch_to: u64,
) -> anyhow::Result<()> {
    if let Some((&previous_from, previous)) = records.range(..=batch_from).next_back() {
        anyhow::ensure!(
            previous.batch_to < batch_from,
            "durable SNARK journal range {batch_from}-{batch_to} overlaps {previous_from}-{}",
            previous.batch_to
        );
    }
    if let Some((&next_from, next)) = records.range(batch_from..).next() {
        anyhow::ensure!(
            batch_to < next_from,
            "durable SNARK journal range {batch_from}-{batch_to} overlaps {next_from}-{}",
            next.batch_to
        );
    }
    Ok(())
}

fn validate_record_structure(record: &DurableSnarkRecord) -> anyhow::Result<()> {
    anyhow::ensure!(
        record.format_version == JOURNAL_FORMAT_VERSION,
        "unsupported durable SNARK journal version {}",
        record.format_version
    );
    anyhow::ensure!(
        record.batch_from <= record.batch_to,
        "invalid journal range"
    );
    let expected_count = record
        .batch_to
        .checked_sub(record.batch_from)
        .and_then(|count| count.checked_add(1))
        .context("journal range length overflow")?;
    anyhow::ensure!(
        (MIN_JOURNALED_FRIS..=MAX_JOURNALED_FRIS).contains(&record.batches.len()),
        "journal aggregate count must be between {MIN_JOURNALED_FRIS} and {MAX_JOURNALED_FRIS}"
    );
    anyhow::ensure!(
        usize::try_from(expected_count).ok() == Some(record.batches.len()),
        "journal range/count mismatch"
    );
    let SnarkProof::Real(real_proof) = &record.proof else {
        anyhow::bail!("fake SNARKs are reconstructible and must not occupy the durable journal");
    };
    anyhow::ensure!(
        !real_proof.proof.is_empty(),
        "journaled SNARK proof is empty"
    );
    // SYSCOIN: Recovery must enforce the same fixed V32 verifier shape as live HTTP admission and
    // L1 calldata construction; accepting a legacy or oversized wrapper here would make restart
    // behavior less strict than the online path.
    anyhow::ensure!(
        real_proof.proof.len() == ZKSYNC_OS_V8_REAL_PROOF_BYTES,
        "journaled V8 SNARK proof must be exactly {ZKSYNC_OS_V8_REAL_PROOF_BYTES} bytes"
    );
    let proving_version = ProvingVersion::try_from(real_proof.proving_execution_version)
        .context("journaled SNARK proving version is unknown")?;
    // SYSCOIN: The V32 server and its sole on-chain verifier slot accept only app-bound V8.
    // Recognizing an old enum value is not sufficient: replaying it would panic during calldata
    // construction or target a verifier that this fresh deployment intentionally does not expose.
    anyhow::ensure!(
        proving_version == ProvingVersion::V8,
        "journaled SNARK proving version must be V8"
    );
    let first = record.batches.first().context("journal has no batches")?;
    let chain_address: Address = first.batch.chain_address;
    let chain_id = first.batch.batch_info.commit_info.chain_id;
    let mut previous = None;
    for (offset, batch) in record.batches.iter().enumerate() {
        let expected_number = record
            .batch_from
            .checked_add(u64::try_from(offset)?)
            .context("journal batch number overflow")?;
        anyhow::ensure!(
            batch.batch_number() == expected_number,
            "journal batches are not contiguous"
        );
        anyhow::ensure!(
            batch.batch.chain_address == chain_address,
            "journal crosses chain addresses"
        );
        anyhow::ensure!(
            batch.batch.batch_info.commit_info.chain_id == chain_id,
            "journal crosses chain ids"
        );
        anyhow::ensure!(
            matches!(batch.data, FriProof::AlreadySubmittedToL1),
            "journal retained FRI bytes or fake proof data"
        );
        // SYSCOIN: Do not deserialize historical commit authorization into the replay path. The
        // canonical committed provider, not stale signature bytes, is the recovery trust anchor.
        anyhow::ensure!(
            matches!(batch.signature_data, BatchSignatureData::AlreadyCommitted),
            "journal retained obsolete or unvalidated commit-signature data"
        );
        anyhow::ensure!(
            batch.batch.proving_version()? == proving_version,
            "journal batch/proof proving-version mismatch"
        );
        anyhow::ensure!(
            batch
                .batch
                .previous_stored_batch_info
                .batch_number
                .checked_add(1)
                == Some(batch.batch_number()),
            "journal predecessor batch number mismatch"
        );
        if let Some(previous_stored) = previous {
            anyhow::ensure!(
                batch.batch.previous_stored_batch_info == previous_stored,
                "journal predecessor metadata is not contiguous"
            );
        }
        previous = Some(batch.batch.batch_info.clone().into_stored());
    }
    Ok(())
}

async fn validate_record_against_committed(
    record: &DurableSnarkRecord,
    chain_id: u64,
    chain_address: Address,
    committed_batches: &CommittedBatchProvider,
) -> anyhow::Result<()> {
    for batch in &record.batches {
        validate_batch_against_committed(
            &batch.batch,
            batch.batch_number(),
            chain_id,
            chain_address,
            committed_batches,
        )
        .await?;
    }
    Ok(())
}

/// SYSCOIN: Validate every durable or recreated FRI against the same V32 settlement facts before
/// queue admission or completed-ownership deduplication. Tombstones prove ownership, not validity.
pub(super) async fn validate_batch_against_committed(
    batch: &BatchMetadata,
    expected_batch_number: u64,
    chain_id: u64,
    chain_address: Address,
    committed_batches: &CommittedBatchProvider,
) -> anyhow::Result<ProvingVersion> {
    anyhow::ensure!(
        expected_batch_number > 0,
        "SNARK recovery cannot aggregate the genesis batch"
    );
    anyhow::ensure!(
        batch.batch_info.commit_info.batch_number == expected_batch_number,
        "SNARK recovery batch number mismatch: expected {expected_batch_number}, got {}",
        batch.batch_info.commit_info.batch_number
    );
    anyhow::ensure!(
        batch.chain_address == chain_address,
        "SNARK recovery batch {expected_batch_number} belongs to settlement chain address {}, expected {chain_address}",
        batch.chain_address
    );
    anyhow::ensure!(
        batch.batch_info.commit_info.chain_id == chain_id,
        "SNARK recovery batch {expected_batch_number} belongs to chain id {}, expected {chain_id}",
        batch.batch_info.commit_info.chain_id
    );

    // SYSCOIN: Committed discovery currently carries no protocol boundary. A fresh V32 chain is
    // homogeneously V8; fail closed on a future protocol until discovery supplies canonical
    // per-batch version metadata instead of trusting evictable local FRI bytes.
    let expected_protocol = ProtocolSemanticVersion::new(0, 32, 0);
    anyhow::ensure!(
        batch.batch_info.protocol_version == expected_protocol,
        "SNARK recovery batch {expected_batch_number} protocol version must be 0.32.0, got {}",
        batch.batch_info.protocol_version
    );
    let proving_version = batch
        .proving_version()
        .context("resolve SNARK recovery proving version")?;
    anyhow::ensure!(
        proving_version == ProvingVersion::V8,
        "V32 SNARK recovery batch {expected_batch_number} must use proving version V8"
    );

    let canonical_predecessor = committed_batches
        .wait_for_batch(expected_batch_number - 1)
        .await;
    anyhow::ensure!(
        batch.previous_stored_batch_info == canonical_predecessor.batch_info,
        "SNARK recovery batch {expected_batch_number} predecessor does not match canonical committed metadata"
    );

    let committed = committed_batches
        .wait_for_batch(expected_batch_number)
        .await;
    let local_stored = batch.batch_info.clone().into_stored();
    anyhow::ensure!(
        committed.batch_info == local_stored,
        "SNARK recovery batch {expected_batch_number} does not match canonical committed metadata"
    );
    anyhow::ensure!(
        committed.block_range.start() == &batch.first_block_number
            && committed.block_range.end() == &batch.last_block_number,
        "SNARK recovery batch {expected_batch_number} block range does not match canonical committed range"
    );

    // SYSCOIN: StoredBatchInfo commits the Gateway execute opening as
    // keccak256(localLogsRoot || multichainRoot). Reconstruct that exact V32 root and the ordered
    // messenger-log/message relation for disk, live, and journal recovery alike.
    validate_execute_metadata_against_committed(batch, &committed.batch_info)?;
    Ok(proving_version)
}

fn validate_execute_metadata_against_committed(
    batch: &BatchMetadata,
    canonical_stored: &StoredBatchInfo,
) -> anyhow::Result<()> {
    let batch_number = batch.batch_info.commit_info.batch_number;
    let mut messages = batch.messages.iter();
    for log in &batch.logs {
        if log.sender != L2_TO_L1_MESSENGER_ADDRESS {
            continue;
        }
        let message = messages.next().with_context(|| {
            format!("journal batch {batch_number} has fewer messages than canonical messenger logs")
        })?;
        anyhow::ensure!(
            log.l2_shard_id == 0,
            "journal batch {batch_number} messenger log has noncanonical L2 shard id"
        );
        anyhow::ensure!(
            log.is_service,
            "journal batch {batch_number} messenger log is not a canonical service log"
        );
        anyhow::ensure!(
            keccak256(message) == log.value,
            "journal batch {batch_number} message does not match its canonical messenger log"
        );
    }
    anyhow::ensure!(
        messages.next().is_none(),
        "journal batch {batch_number} has more messages than canonical messenger logs"
    );

    let reconstructed_root = reconstruct_execute_root(batch)?;
    anyhow::ensure!(
        reconstructed_root == canonical_stored.l2_to_l1_logs_root_hash,
        "journal batch {batch_number} logs/multichain root does not match canonical committed root"
    );
    Ok(())
}

pub(super) fn reconstruct_execute_root(batch: &BatchMetadata) -> anyhow::Result<B256> {
    anyhow::ensure!(
        batch.logs.len() <= L2_TO_L1_TREE_SIZE,
        "journal batch {} has more logs than the canonical L2-to-L1 tree",
        batch.batch_info.commit_info.batch_number
    );
    let log_leaves = batch.logs.iter().map(|log| {
        L2ToL1Log {
            l2_shard_id: log.l2_shard_id,
            is_service: log.is_service,
            tx_number_in_block: log.tx_number_in_batch,
            sender: log.sender,
            key: log.key,
            value: log.value,
        }
        .encode()
    });
    let local_logs_root = MiniMerkleTree::new(log_leaves, Some(L2_TO_L1_TREE_SIZE)).merkle_root();
    let mut root_preimage = [0_u8; 64];
    root_preimage[..32].copy_from_slice(local_logs_root.as_slice());
    root_preimage[32..].copy_from_slice(batch.multichain_root.as_slice());
    Ok(keccak256(root_preimage))
}

#[cfg(test)]
fn validate_canonical_predecessor(
    record: &DurableSnarkRecord,
    canonical_predecessor: &zksync_os_contract_interface::models::StoredBatchInfo,
) -> anyhow::Result<()> {
    let first = record.batches.first().context("journal has no batches")?;
    anyhow::ensure!(
        first.batch.previous_stored_batch_info == *canonical_predecessor,
        "journal first-batch predecessor does not match canonical committed metadata"
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct JournalAllocationLimits {
    min_batches: usize,
    max_batches: usize,
    max_proof_bytes: usize,
    max_logs_per_batch: usize,
    max_messages_per_batch: usize,
    max_message_bytes_per_batch: usize,
    max_operator_da_input_bytes: usize,
    max_edge_da_refs_input_bytes: usize,
}

// SYSCOIN: A streaming first pass applies every dynamic-field bound before serde may allocate the
// real record. The second typed pass is therefore limited by protocol cardinalities and file size.
const JOURNAL_ALLOCATION_LIMITS: JournalAllocationLimits = JournalAllocationLimits {
    min_batches: MIN_JOURNALED_FRIS,
    max_batches: MAX_JOURNALED_FRIS,
    max_proof_bytes: ZKSYNC_OS_V8_REAL_PROOF_BYTES,
    max_logs_per_batch: MAX_JOURNALED_LOGS_PER_BATCH,
    max_messages_per_batch: MAX_JOURNALED_MESSAGES_PER_BATCH,
    max_message_bytes_per_batch: MAX_JOURNALED_MESSAGE_BYTES_PER_BATCH,
    max_operator_da_input_bytes: MAX_JOURNALED_OPERATOR_DA_INPUT_BYTES,
    max_edge_da_refs_input_bytes: MAX_JOURNALED_EDGE_DA_REFS_INPUT_BYTES,
};

fn validate_deserialization_bounds(
    bytes: &[u8],
    limits: JournalAllocationLimits,
) -> anyhow::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    JournalRecordBoundsSeed { limits }
        .deserialize(&mut deserializer)
        .context("validate durable SNARK journal allocation bounds")?;
    deserializer
        .end()
        .context("finish durable SNARK journal allocation-bound validation")?;
    Ok(())
}

struct JournalRecordBoundsSeed {
    limits: JournalAllocationLimits,
}

impl<'de> de::DeserializeSeed<'de> for JournalRecordBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(JournalRecordBoundsVisitor {
            limits: self.limits,
        })
    }
}

struct JournalRecordBoundsVisitor {
    limits: JournalAllocationLimits,
}

impl<'de> Visitor<'de> for JournalRecordBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded durable SNARK journal record")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut saw_batches = false;
        let mut saw_proof = false;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "batches" => {
                    if saw_batches {
                        return Err(A::Error::duplicate_field("batches"));
                    }
                    saw_batches = true;
                    map.next_value_seed(BatchesBoundsSeed {
                        limits: self.limits,
                    })?;
                }
                "proof" => {
                    if saw_proof {
                        return Err(A::Error::duplicate_field("proof"));
                    }
                    saw_proof = true;
                    map.next_value_seed(SnarkProofBoundsSeed {
                        max_bytes: self.limits.max_proof_bytes,
                    })?;
                }
                _ => {
                    let _: IgnoredAny = map.next_value()?;
                }
            }
        }
        if !saw_batches {
            return Err(A::Error::missing_field("batches"));
        }
        if !saw_proof {
            return Err(A::Error::missing_field("proof"));
        }
        Ok(())
    }
}

struct BatchesBoundsSeed {
    limits: JournalAllocationLimits,
}

impl<'de> de::DeserializeSeed<'de> for BatchesBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(BatchesBoundsVisitor {
            limits: self.limits,
        })
    }
}

struct BatchesBoundsVisitor {
    limits: JournalAllocationLimits,
}

impl<'de> Visitor<'de> for BatchesBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a protocol-bounded sequence of journal batches")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut count = 0usize;
        while let Some(()) = sequence.next_element_seed(BatchEnvelopeBoundsSeed {
            limits: self.limits,
        })? {
            count = count
                .checked_add(1)
                .ok_or_else(|| A::Error::custom("journal batch count overflow"))?;
            if count > self.limits.max_batches {
                return Err(A::Error::custom(format_args!(
                    "journal aggregate exceeds {} batches",
                    self.limits.max_batches
                )));
            }
        }
        if count < self.limits.min_batches {
            return Err(A::Error::custom(format_args!(
                "journal aggregate contains {count} batches, minimum is {}",
                self.limits.min_batches
            )));
        }
        Ok(())
    }
}

struct BatchEnvelopeBoundsSeed {
    limits: JournalAllocationLimits,
}

impl<'de> de::DeserializeSeed<'de> for BatchEnvelopeBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(BatchEnvelopeBoundsVisitor {
            limits: self.limits,
        })
    }
}

struct BatchEnvelopeBoundsVisitor {
    limits: JournalAllocationLimits,
}

impl<'de> Visitor<'de> for BatchEnvelopeBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a compact durable FRI envelope")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut saw_batch = false;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "batch" => {
                    if saw_batch {
                        return Err(A::Error::duplicate_field("batch"));
                    }
                    saw_batch = true;
                    map.next_value_seed(BatchMetadataBoundsSeed {
                        limits: self.limits,
                    })?;
                }
                _ => {
                    let _: IgnoredAny = map.next_value()?;
                }
            }
        }
        if !saw_batch {
            return Err(A::Error::missing_field("batch"));
        }
        Ok(())
    }
}

struct BatchMetadataBoundsSeed {
    limits: JournalAllocationLimits,
}

impl<'de> de::DeserializeSeed<'de> for BatchMetadataBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(BatchMetadataBoundsVisitor {
            limits: self.limits,
        })
    }
}

struct BatchMetadataBoundsVisitor {
    limits: JournalAllocationLimits,
}

impl<'de> Visitor<'de> for BatchMetadataBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("protocol-bounded journal batch metadata")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut saw_commit_info = false;
        let mut saw_logs = false;
        let mut message_lengths = None;
        let mut messages_b64_bytes = None;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "commit_batch_info" => {
                    if saw_commit_info {
                        return Err(A::Error::duplicate_field("commit_batch_info"));
                    }
                    saw_commit_info = true;
                    map.next_value_seed(CommitInfoBoundsSeed {
                        limits: self.limits,
                    })?;
                }
                "logs" => {
                    if saw_logs {
                        return Err(A::Error::duplicate_field("logs"));
                    }
                    saw_logs = true;
                    map.next_value_seed(IgnoredSequenceBoundsSeed {
                        field: "batch logs",
                        max_items: self.limits.max_logs_per_batch,
                    })?;
                }
                "message_lengths" => {
                    if message_lengths.is_some() {
                        return Err(A::Error::duplicate_field("message_lengths"));
                    }
                    message_lengths = Some(map.next_value_seed(MessageLengthsBoundsSeed {
                        max_messages: self.limits.max_messages_per_batch,
                        max_total_bytes: self.limits.max_message_bytes_per_batch,
                    })?);
                }
                "messages_b64" => {
                    if messages_b64_bytes.is_some() {
                        return Err(A::Error::duplicate_field("messages_b64"));
                    }
                    messages_b64_bytes = Some(map.next_value_seed(Base64StringBoundsSeed {
                        field: "compact L2-to-L1 messages",
                        max_encoded_bytes: padded_base64_len(
                            self.limits.max_message_bytes_per_batch,
                        ),
                    })?);
                }
                _ => {
                    let _: IgnoredAny = map.next_value()?;
                }
            }
        }
        if !saw_commit_info {
            return Err(A::Error::missing_field("commit_batch_info"));
        }
        let (_, message_bytes) =
            message_lengths.ok_or_else(|| A::Error::missing_field("message_lengths"))?;
        let encoded_bytes =
            messages_b64_bytes.ok_or_else(|| A::Error::missing_field("messages_b64"))?;
        if encoded_bytes != padded_base64_len(message_bytes) {
            return Err(A::Error::custom(
                "compact-message base64 length does not match declared message lengths",
            ));
        }
        Ok(())
    }
}

struct CommitInfoBoundsSeed {
    limits: JournalAllocationLimits,
}

impl<'de> de::DeserializeSeed<'de> for CommitInfoBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(CommitInfoBoundsVisitor {
            limits: self.limits,
        })
    }
}

struct CommitInfoBoundsVisitor {
    limits: JournalAllocationLimits,
}

impl<'de> Visitor<'de> for CommitInfoBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("protocol-bounded journal commit metadata")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut saw_operator_da_input = false;
        let mut saw_edge_da_refs_input = false;
        let mut saw_protocol_version = false;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "operator_da_input" => {
                    if saw_operator_da_input {
                        return Err(A::Error::duplicate_field("operator_da_input"));
                    }
                    saw_operator_da_input = true;
                    map.next_value_seed(BytesBoundsSeed {
                        field: "operator_da_input",
                        max_bytes: self.limits.max_operator_da_input_bytes,
                    })?;
                }
                "edge_da_refs_input" => {
                    if saw_edge_da_refs_input {
                        return Err(A::Error::duplicate_field("edge_da_refs_input"));
                    }
                    saw_edge_da_refs_input = true;
                    map.next_value_seed(BytesBoundsSeed {
                        field: "edge_da_refs_input",
                        max_bytes: self.limits.max_edge_da_refs_input_bytes,
                    })?;
                }
                "protocol_version" => {
                    if saw_protocol_version {
                        return Err(A::Error::duplicate_field("protocol_version"));
                    }
                    saw_protocol_version = true;
                    // SYSCOIN: V32 is the sole fresh-chain protocol identity. Requiring the exact
                    // canonical semver also excludes unbounded prerelease/build metadata strings.
                    map.next_value_seed(ExactStringSeed {
                        field: "durable journal protocol_version",
                        expected: "0.32.0",
                    })?;
                }
                _ => {
                    let _: IgnoredAny = map.next_value()?;
                }
            }
        }
        if !saw_operator_da_input {
            return Err(A::Error::missing_field("operator_da_input"));
        }
        if !saw_edge_da_refs_input {
            return Err(A::Error::missing_field("edge_da_refs_input"));
        }
        if !saw_protocol_version {
            return Err(A::Error::missing_field("protocol_version"));
        }
        Ok(())
    }
}

struct ExactStringSeed {
    field: &'static str,
    expected: &'static str,
}

impl<'de> de::DeserializeSeed<'de> for ExactStringSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_str(ExactStringVisitor {
            field: self.field,
            expected: self.expected,
        })
    }
}

struct ExactStringVisitor {
    field: &'static str,
    expected: &'static str,
}

impl<'de> Visitor<'de> for ExactStringVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} must be {}", self.field, self.expected)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(value)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value == self.expected {
            Ok(())
        } else {
            Err(E::custom(format_args!(
                "{} must be {}",
                self.field, self.expected
            )))
        }
    }
}

struct IgnoredSequenceBoundsSeed {
    field: &'static str,
    max_items: usize,
}

impl<'de> de::DeserializeSeed<'de> for IgnoredSequenceBoundsSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(IgnoredSequenceBoundsVisitor {
            field: self.field,
            max_items: self.max_items,
        })
    }
}

struct IgnoredSequenceBoundsVisitor {
    field: &'static str,
    max_items: usize,
}

impl<'de> Visitor<'de> for IgnoredSequenceBoundsVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a bounded sequence for {}", self.field)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut count = 0usize;
        while sequence.next_element::<IgnoredAny>()?.is_some() {
            count = count
                .checked_add(1)
                .ok_or_else(|| A::Error::custom(format_args!("{} count overflow", self.field)))?;
            if count > self.max_items {
                return Err(A::Error::custom(format_args!(
                    "{} exceeds {} items",
                    self.field, self.max_items
                )));
            }
        }
        Ok(count)
    }
}

struct MessageLengthsBoundsSeed {
    max_messages: usize,
    max_total_bytes: usize,
}

impl<'de> de::DeserializeSeed<'de> for MessageLengthsBoundsSeed {
    type Value = (usize, usize);

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(MessageLengthsBoundsVisitor {
            max_messages: self.max_messages,
            max_total_bytes: self.max_total_bytes,
        })
    }
}

struct MessageLengthsBoundsVisitor {
    max_messages: usize,
    max_total_bytes: usize,
}

impl<'de> Visitor<'de> for MessageLengthsBoundsVisitor {
    type Value = (usize, usize);

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a protocol-bounded sequence of L2-to-L1 message lengths")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut messages = 0usize;
        let mut total_bytes = 0usize;
        while let Some(message_bytes) = sequence.next_element::<u32>()? {
            messages = messages
                .checked_add(1)
                .ok_or_else(|| A::Error::custom("journal message count overflow"))?;
            if messages > self.max_messages {
                return Err(A::Error::custom(format_args!(
                    "journal messages exceed {} entries",
                    self.max_messages
                )));
            }
            total_bytes = total_bytes
                .checked_add(message_bytes as usize)
                .ok_or_else(|| A::Error::custom("journal message byte count overflow"))?;
            if total_bytes > self.max_total_bytes {
                return Err(A::Error::custom(format_args!(
                    "journal message bytes exceed {} per batch",
                    self.max_total_bytes
                )));
            }
        }
        Ok((messages, total_bytes))
    }
}

struct Base64StringBoundsSeed {
    field: &'static str,
    max_encoded_bytes: usize,
}

impl<'de> de::DeserializeSeed<'de> for Base64StringBoundsSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_str(Base64StringBoundsVisitor {
            field: self.field,
            max_encoded_bytes: self.max_encoded_bytes,
        })
    }
}

struct Base64StringBoundsVisitor {
    field: &'static str,
    max_encoded_bytes: usize,
}

impl<'de> Visitor<'de> for Base64StringBoundsVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a bounded base64 string for {}", self.field)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(value)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.len() > self.max_encoded_bytes {
            return Err(E::custom(format_args!(
                "{} exceeds {} encoded bytes",
                self.field, self.max_encoded_bytes
            )));
        }
        Ok(value.len())
    }
}

struct BytesBoundsSeed {
    field: &'static str,
    max_bytes: usize,
}

impl<'de> de::DeserializeSeed<'de> for BytesBoundsSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(BytesBoundsVisitor {
            field: self.field,
            max_bytes: self.max_bytes,
        })
    }
}

struct BytesBoundsVisitor {
    field: &'static str,
    max_bytes: usize,
}

impl<'de> Visitor<'de> for BytesBoundsVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a bounded byte sequence for {}", self.field)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut count = 0usize;
        while sequence.next_element::<u8>()?.is_some() {
            count = count.checked_add(1).ok_or_else(|| {
                A::Error::custom(format_args!("{} byte count overflow", self.field))
            })?;
            if count > self.max_bytes {
                return Err(A::Error::custom(format_args!(
                    "{} exceeds {} bytes",
                    self.field, self.max_bytes
                )));
            }
        }
        Ok(count)
    }
}

struct SnarkProofBoundsSeed {
    max_bytes: usize,
}

impl<'de> de::DeserializeSeed<'de> for SnarkProofBoundsSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(SnarkProofBoundsVisitor {
            max_bytes: self.max_bytes,
        })
    }
}

struct SnarkProofBoundsVisitor {
    max_bytes: usize,
}

impl<'de> Visitor<'de> for SnarkProofBoundsVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a compact real, bounded SNARK proof")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut saw_proof_b64 = false;
        let mut saw_proving_version = false;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "proof_b64" => {
                    if saw_proof_b64 {
                        return Err(A::Error::duplicate_field("proof_b64"));
                    }
                    saw_proof_b64 = true;
                    let encoded_bytes = map.next_value_seed(Base64StringBoundsSeed {
                        field: "SNARK proof",
                        max_encoded_bytes: padded_base64_len(self.max_bytes),
                    })?;
                    if encoded_bytes != padded_base64_len(self.max_bytes) {
                        return Err(A::Error::custom(
                            "journaled SNARK proof base64 has invalid length",
                        ));
                    }
                }
                "proving_execution_version" => {
                    if saw_proving_version {
                        return Err(A::Error::duplicate_field("proving_execution_version"));
                    }
                    saw_proving_version = true;
                    let _: u32 = map.next_value()?;
                }
                _ => {
                    let _: IgnoredAny = map.next_value()?;
                }
            }
        }
        if !saw_proof_b64 {
            return Err(A::Error::missing_field("proof_b64"));
        }
        if !saw_proving_version {
            return Err(A::Error::missing_field("proving_execution_version"));
        }
        Ok(())
    }
}

async fn load_record(path: &Path) -> anyhow::Result<DurableSnarkRecord> {
    let bytes = read_bounded_journal_file(path).await?;
    let version: JournalVersionProbe = serde_json::from_slice(&bytes)
        .with_context(|| format!("read durable SNARK journal version {}", path.display()))?;
    if version.format_version == RETIRED_JOURNAL_FORMAT_VERSION {
        anyhow::bail!(
            "durable SNARK journal {} uses retired pre-mainnet V1 format; stop the node and reset the retired V31 testnet proof-storage/snark_journal directory",
            path.display()
        );
    }
    anyhow::ensure!(
        version.format_version == JOURNAL_FORMAT_VERSION,
        "unsupported durable SNARK journal version {} in {}",
        version.format_version,
        path.display()
    );
    // SYSCOIN: Reject allocation bombs in marker/proof/metadata collections before constructing
    // any attacker-controlled Vec from a crash-surviving journal file.
    validate_deserialization_bounds(&bytes, JOURNAL_ALLOCATION_LIMITS)
        .with_context(|| format!("bound durable SNARK journal {}", path.display()))?;
    let record: DurableSnarkRecordV2 = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode durable SNARK journal {}", path.display()))?;
    record
        .into_record()
        .with_context(|| format!("expand durable SNARK journal {}", path.display()))
}

fn serialize_record(record: &DurableSnarkRecord) -> anyhow::Result<Vec<u8>> {
    serialize_record_with_limit(record, MAX_JOURNAL_RECORD_BYTES)
}

fn serialize_record_with_limit(
    record: &DurableSnarkRecord,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    let durable = DurableSnarkRecordV2::from_record(record)?;
    let mut writer = SizeLimitedWriter::new(limit);
    serde_json::to_writer(&mut writer, &durable)
        .context("serialize bounded durable SNARK journal")?;
    Ok(writer.into_inner())
}

struct SizeLimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl SizeLimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::io::Write for SizeLimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let new_len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|new_len| *new_len <= self.limit)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("durable SNARK journal exceeds {} bytes", self.limit),
                )
            })?;
        if new_len > self.bytes.capacity() {
            // SYSCOIN: Grow geometrically within the honest hard cap. Reserving exactly each
            // serde token would turn a large-but-valid metadata record into quadratic copying.
            let next_capacity = self
                .bytes
                .capacity()
                .max(4 * 1024)
                .saturating_mul(2)
                .max(new_len)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(next_capacity - self.bytes.len())
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::OutOfMemory,
                        format!("reserve durable SNARK journal buffer: {error}"),
                    )
                })?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn read_bounded_journal_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let path_metadata = verify_owner_only_regular_file(path).await?;
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("open durable SNARK journal {}", path.display()))?;
    let metadata = file.metadata().await?;
    verify_owner_only_file_metadata(path, &metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        anyhow::ensure!(
            metadata.dev() == path_metadata.dev() && metadata.ino() == path_metadata.ino(),
            "durable SNARK journal changed while opening: {}",
            path.display()
        );
    }
    anyhow::ensure!(
        metadata.len() <= MAX_JOURNAL_RECORD_BYTES as u64,
        "durable SNARK journal exceeds {MAX_JOURNAL_RECORD_BYTES} bytes: {}",
        path.display()
    );
    let capacity = usize::try_from(metadata.len()).context("journal file length exceeds usize")?;
    let mut bytes = vec![0_u8; capacity];
    file.read_exact(&mut bytes).await?;
    let mut trailing = [0_u8; 1];
    let trailing_bytes = file.read(&mut trailing).await?;
    anyhow::ensure!(
        trailing_bytes == 0,
        "durable SNARK journal grew beyond {MAX_JOURNAL_RECORD_BYTES} bytes while reading: {}",
        path.display()
    );
    Ok(bytes)
}

async fn durable_publish_new(directory: &Path, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let transaction = JOURNAL_TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary_path = directory.join(format!(
        "{JOURNAL_TEMP_PREFIX}{}-{transaction}{JOURNAL_TEMP_SUFFIX}",
        std::process::id()
    ));
    let final_path = directory.join(key);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary_path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        // SYSCOIN: `OpenOptions::mode` is filtered by the process umask. Normalize and verify the
        // actual descriptor/path before proof bytes can be published under a canonical name.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .await?;
        let opened = file.metadata().await?;
        let linked = fs::symlink_metadata(&temporary_path).await?;
        verify_owner_only_file_metadata(&temporary_path, &opened)?;
        verify_owner_only_file_metadata(&temporary_path, &linked)?;
        anyhow::ensure!(
            opened.dev() == linked.dev() && opened.ino() == linked.ino(),
            "SNARK journal transaction temp changed while opening: {}",
            temporary_path.display()
        );
    }
    if let Err(error) = file.write_all(bytes).await {
        drop(file);
        cleanup_unpublished_temp(directory, &temporary_path).await;
        return Err(error.into());
    }
    if let Err(error) = file.sync_all().await {
        drop(file);
        cleanup_unpublished_temp(directory, &temporary_path).await;
        return Err(error.into());
    }
    drop(file);

    // SYSCOIN: Hard-link publication is atomic and refuses to overwrite a conflicting final file.
    if let Err(error) = fs::hard_link(&temporary_path, &final_path).await {
        cleanup_unpublished_temp(directory, &temporary_path).await;
        return Err(error.into());
    }
    // SYSCOIN: Only this fsync is on the acceptance critical path. If it fails, the prover keeps
    // the exact proof and retries; either the final link or temp is available for conservative
    // retry/restart discovery.
    sync_directory(directory).await?;

    // SYSCOIN: Once final publication is durable, temp cleanup cannot invalidate acceptance. A
    // retained same-inode alias is identified against the final link and removed on restart.
    if let Err(error) = fs::remove_file(&temporary_path).await {
        tracing::warn!(
            path = temporary_path.display().to_string(),
            ?error,
            "durable SNARK journal published but transaction temp cleanup failed"
        );
        return Ok(());
    }
    if let Err(error) = sync_directory(directory).await {
        tracing::warn!(
            path = directory.display().to_string(),
            ?error,
            "durable SNARK journal published but temp-unlink fsync failed"
        );
    }
    Ok(())
}

async fn cleanup_unpublished_temp(directory: &Path, temporary_path: &Path) {
    // SYSCOIN: Best-effort cleanup prevents repeated ENOSPC retries from accumulating partial
    // proof copies. Failure is safe: startup quarantines every unacknowledged transaction temp.
    if fs::remove_file(temporary_path).await.is_ok() {
        let _ = sync_directory(directory).await;
    }
}

async fn quarantine_transaction_temps(directory: &Path) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory).await?;
    let mut temporary_paths = Vec::new();
    let mut published_metadata = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(JOURNAL_TEMP_PREFIX) && name.ends_with(JOURNAL_TEMP_SUFFIX) {
            let metadata = verify_owner_only_regular_file(&entry.path()).await?;
            temporary_paths.push((entry.path(), metadata));
            continue;
        }
        // SYSCOIN: A transaction temp hard-linked to a canonical final name is already published;
        // retain only the final link. Non-canonical/conflicting files still fail startup below.
        if let Some((batch_from, batch_to)) = parse_journal_key(&name)
            && *name == journal_key(batch_from, batch_to)
        {
            published_metadata.push(verify_owner_only_regular_file(&entry.path()).await?);
        }
    }
    if temporary_paths.is_empty() {
        return Ok(());
    }
    let mut quarantine: Option<PathBuf> = None;
    for (path, temp_metadata) in temporary_paths {
        if published_metadata
            .iter()
            .any(|published| same_file_identity(&temp_metadata, published))
        {
            fs::remove_file(&path).await?;
            tracing::info!(
                path = path.display().to_string(),
                "removed published SNARK journal transaction-temp alias"
            );
            continue;
        }
        let quarantine = match &quarantine {
            Some(path) => path.clone(),
            None => {
                let path = directory.join(JOURNAL_QUARANTINE_DIR);
                fs::create_dir_all(&path).await?;
                set_owner_only_directory(&path).await?;
                quarantine = Some(path.clone());
                path
            }
        };
        let filename = path.file_name().context("journal temp has no filename")?;
        // SYSCOIN: PIDs and the per-process counter may be reused after enough restarts. Preserve
        // every unpublished forensic copy instead of allowing Unix `rename` to overwrite one.
        let destination = unused_quarantine_destination(&quarantine, filename).await?;
        fs::rename(&path, &destination).await?;
        tracing::warn!(
            path = destination.display().to_string(),
            "quarantined unacknowledged SNARK journal transaction from interrupted publication"
        );
    }
    if let Some(quarantine) = quarantine {
        sync_directory(&quarantine).await?;
    }
    sync_directory(directory).await?;
    Ok(())
}

async fn unused_quarantine_destination(
    quarantine: &Path,
    filename: &std::ffi::OsStr,
) -> anyhow::Result<PathBuf> {
    for suffix in 0..=u32::MAX {
        let candidate = if suffix == 0 {
            quarantine.join(filename)
        } else {
            quarantine.join(format!(
                "{}.quarantine-{suffix}",
                filename.to_string_lossy()
            ))
        };
        match fs::symlink_metadata(&candidate).await {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!(
        "exhausted collision-safe SNARK journal quarantine names for {}",
        filename.to_string_lossy()
    )
}

fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}

async fn acquire_process_lock(directory: &Path) -> anyhow::Result<JournalProcessLock> {
    let path = directory.join(JOURNAL_PROCESS_LOCK_FILE);
    #[cfg(unix)]
    {
        let display_path = path.clone();
        let file = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
            let file = options
                .open(&path)
                .with_context(|| format!("open durable SNARK journal lock {}", path.display()))?;
            let opened = file.metadata()?;
            anyhow::ensure!(
                opened.file_type().is_file(),
                "SNARK journal lock is not a regular file: {}",
                path.display()
            );
            // SAFETY: `geteuid` has no preconditions and only reads the process credential.
            let effective_uid = unsafe { libc::geteuid() };
            anyhow::ensure!(
                opened.uid() == effective_uid,
                "SNARK journal lock {} is owned by uid {}, current effective uid is {}",
                path.display(),
                opened.uid(),
                effective_uid
            );
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let opened = file.metadata()?;
            let linked = std::fs::symlink_metadata(&path)?;
            verify_owner_only_file_metadata(&path, &opened)?;
            verify_owner_only_file_metadata(&path, &linked)?;
            anyhow::ensure!(
                opened.dev() == linked.dev() && opened.ino() == linked.ino(),
                "SNARK journal lock changed while opening: {}",
                path.display()
            );
            // SAFETY: `file` is a live descriptor and `flock` does not retain the pointer state.
            let lock_result =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if lock_result != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    anyhow::bail!(
                        "durable SNARK journal is already locked by another process at {}: {error}",
                        path.display()
                    );
                }
                return Err(error).with_context(|| {
                    format!("acquire durable SNARK journal lock {}", path.display())
                });
            }
            Ok(file)
        })
        .await
        .with_context(|| {
            format!(
                "join durable SNARK journal lock task {}",
                display_path.display()
            )
        })??;
        Ok(JournalProcessLock { _file: file })
    }
    #[cfg(not(unix))]
    anyhow::bail!(
        "durable SNARK journal requires Unix process locking at {}",
        path.display()
    );
}

async fn set_owner_only_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = fs::symlink_metadata(path).await?;
        anyhow::ensure!(
            metadata.file_type().is_dir(),
            "SNARK journal path is not a directory: {}",
            path.display()
        );
        // SAFETY: `geteuid` has no preconditions and only reads the process credential.
        let effective_uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            metadata.uid() == effective_uid,
            "SNARK journal directory {} is owned by uid {}, current effective uid is {}",
            path.display(),
            metadata.uid(),
            effective_uid
        );
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
        let restricted = fs::symlink_metadata(path).await?;
        anyhow::ensure!(
            restricted.uid() == effective_uid && restricted.mode() & 0o777 == 0o700,
            "SNARK journal directory {} is not owner-only mode 0700",
            path.display()
        );
    }
    #[cfg(not(unix))]
    anyhow::ensure!(
        fs::symlink_metadata(path).await?.file_type().is_dir(),
        "SNARK journal path is not a directory: {}",
        path.display()
    );
    Ok(())
}

async fn verify_owner_only_regular_file(path: &Path) -> anyhow::Result<std::fs::Metadata> {
    let metadata = fs::symlink_metadata(path).await?;
    verify_owner_only_file_metadata(path, &metadata)?;
    Ok(metadata)
}

fn verify_owner_only_file_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "SNARK journal entry is not a regular file: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` has no preconditions and only reads the process credential.
        let effective_uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            metadata.uid() == effective_uid,
            "SNARK journal file {} is owned by uid {}, current effective uid is {}",
            path.display(),
            metadata.uid(),
            effective_uid
        );
        anyhow::ensure!(
            metadata.mode() & 0o777 == 0o600,
            "SNARK journal file {} is not owner-only mode 0600",
            path.display()
        );
    }
    Ok(())
}

async fn sync_directory(path: &Path) -> anyhow::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = std::fs::File::open(&path)?;
        directory.sync_all()
    })
    .await
    .context("join directory fsync task")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover_api::test_util::create_test_batch_envelope_with_data;
    use alloy::primitives::B256;
    use std::time::Duration;
    use tempfile::TempDir;
    use zksync_os_batch_types::batcher_model::RealSnarkProof;
    use zksync_os_l1_sender::commands::SendToL1 as _;
    use zksync_os_types::ProtocolSemanticVersion;

    struct RejectingStartupValidator(std::sync::atomic::AtomicBool);

    impl StartupRecordValidator for RejectingStartupValidator {
        async fn validate(&self, _record: &DurableSnarkRecord) -> anyhow::Result<()> {
            self.0.store(true, Ordering::SeqCst);
            anyhow::bail!("historical pending record reached current-topology validation")
        }
    }

    struct CanonicalExecuteStartupValidator {
        batches: Vec<StoredBatchInfo>,
    }

    impl StartupRecordValidator for CanonicalExecuteStartupValidator {
        async fn validate(&self, record: &DurableSnarkRecord) -> anyhow::Result<()> {
            anyhow::ensure!(
                record.batches.len() == self.batches.len(),
                "canonical execute fixture batch count mismatch"
            );
            for (batch, canonical) in record.batches.iter().zip(&self.batches) {
                anyhow::ensure!(
                    batch.batch_number() == canonical.batch_number,
                    "canonical execute fixture batch number mismatch"
                );
                validate_execute_metadata_against_committed(&batch.batch, canonical)?;
            }
            Ok(())
        }
    }

    fn real_wrapper() -> SnarkProof {
        SnarkProof::Real(RealSnarkProof {
            proof: vec![0x42; ZKSYNC_OS_V8_REAL_PROOF_BYTES],
            proving_execution_version: ProvingVersion::V8 as u32,
        })
    }

    fn assert_allocation_bound_error(
        bytes: &[u8],
        limits: JournalAllocationLimits,
        expected: &str,
    ) {
        let error = validate_deserialization_bounds(bytes, limits).unwrap_err();
        let chain = format!("{error:#}");
        assert!(
            chain.contains(expected),
            "expected `{expected}` in allocation-bound error: {chain}"
        );
    }

    fn marker_batches(from: u64, to: u64) -> Vec<SignedBatchEnvelope<FriProof>> {
        let protocol_version = ProtocolSemanticVersion::new(0, 32, 0);
        let mut batches = Vec::new();
        let mut previous = None;
        for batch_number in from..=to {
            let mut batch = create_test_batch_envelope_with_data(
                batch_number,
                protocol_version.clone(),
                FriProof::AlreadySubmittedToL1,
            );
            if let Some(previous) = previous {
                batch.batch.previous_stored_batch_info = previous;
            }
            batch.signature_data = BatchSignatureData::AlreadyCommitted;
            previous = Some(batch.batch.batch_info.clone().into_stored());
            batches.push(batch);
        }
        batches
    }

    fn canonically_bound_execute_batches() -> anyhow::Result<Vec<SignedBatchEnvelope<FriProof>>> {
        let mut batches = marker_batches(1, 2);
        let message = vec![0x01, 0x12, 0x34];
        batches[0].batch.logs = vec![
            L2Log {
                l2_shard_id: 0,
                is_service: true,
                tx_number_in_batch: 0,
                sender: Address::repeat_byte(0x11),
                key: B256::repeat_byte(0x22),
                value: B256::repeat_byte(0x33),
            },
            L2Log {
                l2_shard_id: 0,
                is_service: true,
                tx_number_in_batch: 1,
                sender: L2_TO_L1_MESSENGER_ADDRESS,
                key: B256::repeat_byte(0x44),
                value: keccak256(&message),
            },
        ];
        batches[0].batch.messages = vec![message];
        batches[0].batch.multichain_root = B256::repeat_byte(0x55);
        batches[0]
            .batch
            .batch_info
            .commit_info
            .l2_to_l1_logs_root_hash = reconstruct_execute_root(&batches[0].batch)?;
        batches[1].batch.previous_stored_batch_info =
            batches[0].batch.batch_info.clone().into_stored();
        batches[1].batch.multichain_root = B256::repeat_byte(0x66);
        batches[1]
            .batch
            .batch_info
            .commit_info
            .l2_to_l1_logs_root_hash = reconstruct_execute_root(&batches[1].batch)?;
        Ok(batches)
    }

    async fn assert_recovery_rejects_execute_mutation(
        mutate: impl FnOnce(&mut serde_json::Value),
        expected_error: &str,
    ) -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let batches = canonically_bound_execute_batches()?;
        let canonical_batches = batches
            .iter()
            .map(|batch| batch.batch.batch_info.clone().into_stored())
            .collect();
        let journaled = journal.persist(batches, real_wrapper()).await?;
        let path = temp.path().join("snark_journal").join(&journaled.key);
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).await?)?;
        mutate(&mut value);
        // SYSCOIN: These regressions retain valid bounded V2 JSON so failure is attributable to
        // canonical execute-metadata binding, not syntax or allocation guards.
        fs::write(&path, serde_json::to_vec(&value)?).await?;

        let validator = CanonicalExecuteStartupValidator {
            batches: canonical_batches,
        };
        let error = match journal
            .recover_with_validator(0, 0, 2, true, &validator)
            .await
        {
            Ok(_) => anyhow::bail!("mutated execute metadata unexpectedly recovered"),
            Err(error) => error,
        };
        let chain = format!("{error:#}");
        assert!(
            chain.contains(expected_error),
            "expected `{expected_error}` in recovery error: {chain}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn published_wrapper_survives_restart_and_confirmation_ack_is_durable()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let journaled = journal
            .persist(marker_batches(1, 2), real_wrapper())
            .await?;
        let key = journaled.key.clone();
        let final_path = temp.path().join("snark_journal").join(&key);
        assert!(fs::try_exists(&final_path).await?);

        // SYSCOIN: Reopening after the publication/completion crash boundary must retain the exact
        // self-contained wrapper even though no in-memory prover job survives.
        drop(journaled);
        drop(confirmations);
        drop(journal);
        let (reopened, reopened_confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert_eq!(reopened.inner.records.lock().await.len(), 1);

        let replay = load_record(&final_path).await?.into_journaled(key.clone());
        let reaper = tokio::spawn(reopened.clone().run_reaper(reopened_confirmations));
        replay
            .into_command(reopened.confirmation_sender())
            .notify_confirmed();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if reopened.inner.records.lock().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(!fs::try_exists(&final_path).await?);
        reopened.remove_confirmed(&key).await?;
        reaper.abort();
        let _ = reaper.await;
        drop(reopened);

        let (after_ack, _after_ack_confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert!(after_ack.inner.records.lock().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn closed_confirmation_channel_retains_journal() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        drop(confirmations);
        let journaled = journal
            .persist(marker_batches(1, 2), real_wrapper())
            .await?;
        let key = journaled.key.clone();
        let path = temp.path().join("snark_journal").join(key);

        journaled
            .into_command(journal.confirmation_sender())
            .notify_confirmed();

        assert_eq!(journal.record_count().await, 1);
        assert!(fs::try_exists(path).await?);
        Ok(())
    }

    #[tokio::test]
    async fn identical_retry_is_idempotent_but_overlap_is_rejected() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        journal
            .persist(marker_batches(1, 2), real_wrapper())
            .await?;
        journal
            .persist(marker_batches(1, 2), real_wrapper())
            .await?;
        assert_eq!(journal.inner.records.lock().await.len(), 1);

        let error = journal
            .persist(marker_batches(2, 3), real_wrapper())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("overlaps"));
        assert_eq!(journal.inner.records.lock().await.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn malformed_final_record_fails_restart_closed() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let journaled = journal
            .persist(marker_batches(7, 8), real_wrapper())
            .await?;
        fs::write(
            temp.path().join("snark_journal").join(&journaled.key),
            b"{not-json",
        )
        .await?;
        drop(journaled);
        drop(confirmations);
        drop(journal);

        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains("read durable SNARK journal version"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restart_rejects_overlapping_final_records() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        fs::create_dir_all(&directory).await?;
        for (from, to) in [(1, 3), (3, 4)] {
            let record = DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: from,
                batch_to: to,
                batches: marker_batches(from, to),
                proof: real_wrapper(),
            };
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let mut file = options.open(directory.join(journal_key(from, to))).await?;
            file.write_all(&serialize_record(&record)?).await?;
            file.sync_all().await?;
        }

        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains("overlaps"));
        Ok(())
    }

    // SYSCOIN: V2 persists no FRI or commit-authorization enum at all. Recovery synthesizes only
    // the two typed marker variants after the bounded compact payload has decoded successfully.
    #[test]
    fn compact_v2_roundtrip_synthesizes_only_safe_markers() -> anyhow::Result<()> {
        let mut batches = marker_batches(1, 2);
        batches[0].batch.messages = vec![Vec::new(), vec![0, 1, 2, 127, 128, 254, 255]];
        let expected_messages = batches[0].batch.messages.clone();

        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches,
            proof: real_wrapper(),
        };
        let serialized = serialize_record(&record)?;
        let json = std::str::from_utf8(&serialized)?;
        assert!(json.contains("\"message_lengths\":[0,7]"));
        assert!(json.contains("\"messages_b64\":\"AAECf4D+/w==\""));
        assert!(!json.contains("\"messages\":["));
        assert!(!json.contains("AlreadySubmittedToL1"));
        assert!(!json.contains("AlreadyCommitted"));

        validate_deserialization_bounds(&serialized, JOURNAL_ALLOCATION_LIMITS)?;
        let roundtrip =
            serde_json::from_slice::<DurableSnarkRecordV2>(&serialized)?.into_record()?;
        assert_eq!(roundtrip.batches[0].batch.messages, expected_messages);
        assert!(
            roundtrip
                .batches
                .iter()
                .all(|batch| matches!(batch.data, FriProof::AlreadySubmittedToL1))
        );
        assert!(
            roundtrip
                .batches
                .iter()
                .all(|batch| matches!(batch.signature_data, BatchSignatureData::AlreadyCommitted))
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_rejects_valid_json_log_mutation_against_committed_root() -> anyhow::Result<()>
    {
        assert_recovery_rejects_execute_mutation(
            |value| {
                value["batches"][0]["batch"]["logs"][0]["value"] =
                    serde_json::to_value(B256::repeat_byte(0x77)).unwrap();
            },
            "logs/multichain root does not match canonical committed root",
        )
        .await
    }

    #[tokio::test]
    async fn recovery_rejects_valid_json_message_mutation_against_messenger_log()
    -> anyhow::Result<()> {
        assert_recovery_rejects_execute_mutation(
            |value| {
                value["batches"][0]["batch"]["messages_b64"] =
                    serde_json::Value::String(general_purpose::STANDARD.encode([0x01, 0x12, 0x35]));
            },
            "message does not match its canonical messenger log",
        )
        .await
    }

    #[tokio::test]
    async fn recovery_rejects_valid_json_multichain_root_mutation_against_committed_root()
    -> anyhow::Result<()> {
        assert_recovery_rejects_execute_mutation(
            |value| {
                value["batches"][0]["batch"]["multichain_root"] =
                    serde_json::to_value(B256::repeat_byte(0x88)).unwrap();
            },
            "logs/multichain root does not match canonical committed root",
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unacknowledged_transaction_temp_is_quarantined() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        fs::create_dir_all(&directory).await?;
        let interrupted = directory.join(format!("{JOURNAL_TEMP_PREFIX}test{JOURNAL_TEMP_SUFFIX}"));
        fs::write(&interrupted, b"partial").await?;
        fs::set_permissions(&interrupted, std::fs::Permissions::from_mode(0o600)).await?;

        let (_journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert!(!fs::try_exists(&interrupted).await?);
        assert!(
            fs::try_exists(
                directory
                    .join(JOURNAL_QUARANTINE_DIR)
                    .join(interrupted.file_name().unwrap())
            )
            .await?
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quarantine_name_reuse_preserves_both_unpublished_temps() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        let quarantine = directory.join(JOURNAL_QUARANTINE_DIR);
        fs::create_dir_all(&quarantine).await?;
        let filename = format!("{JOURNAL_TEMP_PREFIX}reused{JOURNAL_TEMP_SUFFIX}");
        let interrupted = directory.join(&filename);
        let older = quarantine.join(&filename);
        fs::write(&interrupted, b"newer-unpublished").await?;
        fs::write(&older, b"older-unpublished").await?;
        fs::set_permissions(&interrupted, std::fs::Permissions::from_mode(0o600)).await?;
        fs::set_permissions(&older, std::fs::Permissions::from_mode(0o600)).await?;

        let (_journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert_eq!(fs::read(&older).await?, b"older-unpublished");
        assert_eq!(
            fs::read(quarantine.join(format!("{filename}.quarantine-1"))).await?,
            b"newer-unpublished"
        );
        assert!(!fs::try_exists(interrupted).await?);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn published_transaction_temp_alias_is_unlinked_not_quarantined() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        fs::create_dir_all(&directory).await?;
        let final_path = directory.join(journal_key(1, 2));
        let temp_path = directory.join(format!(
            "{JOURNAL_TEMP_PREFIX}published{JOURNAL_TEMP_SUFFIX}"
        ));
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(&temp_path).await?;
        file.write_all(&serialize_record(&record)?).await?;
        file.sync_all().await?;
        drop(file);
        fs::hard_link(&temp_path, &final_path).await?;

        let (_journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert!(!fs::try_exists(&temp_path).await?);
        assert!(fs::try_exists(&final_path).await?);
        assert!(
            !fs::try_exists(
                directory
                    .join(JOURNAL_QUARANTINE_DIR)
                    .join(temp_path.file_name().unwrap())
            )
            .await?
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn journal_process_lock_is_exclusive_and_releases_with_last_clone() -> anyhow::Result<()>
    {
        use std::os::unix::fs::MetadataExt as _;

        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let lock_metadata = fs::symlink_metadata(
            temp.path()
                .join("snark_journal")
                .join(JOURNAL_PROCESS_LOCK_FILE),
        )
        .await?;
        assert_eq!(lock_metadata.mode() & 0o777, 0o600);
        // SAFETY: `geteuid` has no preconditions and only reads the process credential.
        assert_eq!(lock_metadata.uid(), unsafe { libc::geteuid() });
        let clone = journal.clone();
        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains("already locked"));

        drop(confirmations);
        drop(journal);
        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains("already locked"));
        drop(clone);

        let (_reopened, _reopened_confirmations) = SnarkProofJournal::open(temp.path()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn reaper_does_not_keep_its_own_confirmation_channel_open() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let error = tokio::time::timeout(Duration::from_secs(2), journal.run_reaper(confirmations))
            .await?
            .unwrap_err();
        assert!(error.to_string().contains("confirmation channel closed"));
        Ok(())
    }

    #[tokio::test]
    async fn startup_frontier_retires_only_whole_ranges_and_preserves_partial_ranges()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, _confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let historical_proxy = Address::new([0x44; 20]);
        let mut historical_batches = marker_batches(10, 20);
        for batch in &mut historical_batches {
            batch.batch.chain_address = historical_proxy;
        }
        let journaled = journal.persist(historical_batches, real_wrapper()).await?;
        let key = journaled.key.clone();
        let path = temp.path().join("snark_journal").join(&key);
        let record = load_record(&path).await?;
        assert!(
            record
                .batches
                .iter()
                .all(|batch| batch.batch.chain_address == historical_proxy)
        );
        let entry = journal
            .inner
            .records
            .lock()
            .await
            .get(&10)
            .cloned()
            .expect("persisted journal entry");

        let error = journal
            .classify_startup_record(&entry, &record, 15, 15)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("partially covers"));
        assert_eq!(journal.record_count().await, 1);
        assert!(fs::try_exists(&path).await?);

        let validator = RejectingStartupValidator(std::sync::atomic::AtomicBool::new(false));
        let recovered = journal
            .recover_with_validator(20, 15, 20, true, &validator)
            .await?;
        assert!(recovered.replay.is_empty());
        assert_eq!(recovered.pending_confirmation.len(), 1);
        assert_eq!(recovered.pending_confirmation[0].key, key);
        assert_eq!(recovered.pending_confirmation[0].batch_range(), (10, 20));
        assert!(!validator.0.load(Ordering::SeqCst));
        assert_eq!(journal.record_count().await, 1);
        assert!(fs::try_exists(&path).await?);

        assert_eq!(
            journal
                .classify_startup_record(&entry, &record, 20, 15)
                .await?,
            StartupJournalDisposition::PendingConfirmation
        );
        // SYSCOIN: This latest-covered disposition is intentionally independent of the node's
        // current settlement proxy; the historical wrapper is retained but will never replay.
        assert_eq!(journal.record_count().await, 1);
        assert!(fs::try_exists(&path).await?);

        assert_eq!(
            journal
                .classify_startup_record(&entry, &record, 20, 20)
                .await?,
            StartupJournalDisposition::Retired
        );
        assert_eq!(journal.record_count().await, 0);
        assert!(!fs::try_exists(&path).await?);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retired_v1_record_fails_closed_with_operator_reset_instruction() -> anyhow::Result<()>
    {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        fs::create_dir_all(&directory).await?;
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        };
        let mut value = serde_json::to_value(DurableSnarkRecordV2::from_record(&record)?)?;
        value["format_version"] = serde_json::json!(RETIRED_JOURNAL_FORMAT_VERSION);
        let path = directory.join(journal_key(1, 2));
        fs::write(&path, serde_json::to_vec(&value)?).await?;
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;

        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("retired pre-mainnet V1 format"));
        assert!(chain.contains("reset the retired V31 testnet"));
        Ok(())
    }

    #[test]
    fn record_bounds_reject_singleton_oversized_aggregate_and_wrapper() {
        let singleton = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 1,
            batches: marker_batches(1, 1),
            proof: real_wrapper(),
        };
        assert!(
            validate_record_structure(&singleton)
                .unwrap_err()
                .to_string()
                .contains("aggregate count")
        );

        let oversized_aggregate = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: (MAX_JOURNALED_FRIS as u64) + 1,
            batches: marker_batches(1, (MAX_JOURNALED_FRIS as u64) + 1),
            proof: real_wrapper(),
        };
        assert!(
            validate_record_structure(&oversized_aggregate)
                .unwrap_err()
                .to_string()
                .contains("aggregate count")
        );

        let oversized_wrapper = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: SnarkProof::Real(RealSnarkProof {
                proof: vec![0x42; ZKSYNC_OS_V8_REAL_PROOF_BYTES + 32],
                proving_execution_version: ProvingVersion::V8 as u32,
            }),
        };
        assert!(
            validate_record_structure(&oversized_wrapper)
                .unwrap_err()
                .to_string()
                .contains("must be exactly")
        );

        let old_version = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: SnarkProof::Real(RealSnarkProof {
                proof: vec![0x42; ZKSYNC_OS_V8_REAL_PROOF_BYTES],
                proving_execution_version: 7,
            }),
        };
        assert!(
            validate_record_structure(&old_version)
                .unwrap_err()
                .to_string()
                .contains("proving version is unknown")
        );
    }

    #[test]
    fn serialization_limit_is_enforced_before_unbounded_growth() {
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        };
        let error = serialize_record_with_limit(&record, 64).unwrap_err();
        assert!(error.to_string().contains("serialize bounded"));
    }

    #[test]
    fn prelease_record_bound_matches_worst_case_json_exactly() {
        let batches = marker_batches(9, 10);
        let batch_json_bytes = batches
            .iter()
            .map(|batch| durable_snark_batch_json_bytes(&batch.batch).unwrap())
            .sum();
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 9,
            batch_to: 10,
            batches,
            proof: SnarkProof::Real(RealSnarkProof {
                proof: vec![u8::MAX; ZKSYNC_OS_V8_REAL_PROOF_BYTES],
                proving_execution_version: ProvingVersion::V8 as u32,
            }),
        };
        let serialized = serialize_record(&record).unwrap();
        assert_eq!(
            durable_snark_record_json_upper_bound(9, 10, 2, batch_json_bytes),
            Some(serialized.len())
        );
    }

    #[test]
    fn journaled_proof_debug_redacts_proof_bytes() {
        let journaled = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        }
        .into_journaled(journal_key(1, 2));
        let debug = format!("{journaled:?}");
        assert!(debug.contains(&format!(
            "proof_bytes: Some({ZKSYNC_OS_V8_REAL_PROOF_BYTES})"
        )));
        assert!(!debug.contains("66, 66"));
    }

    #[test]
    fn deserialization_probe_bounds_batches_proof_and_nested_collections_before_typed_decode() {
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        };
        let bytes = serialize_record(&record).unwrap();
        validate_deserialization_bounds(&bytes, JOURNAL_ALLOCATION_LIMITS).unwrap();

        let noncanonical_version = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("\"0.32.0\"", "\"0.32.0-alpha-unbounded\"");
        assert_allocation_bound_error(
            noncanonical_version.as_bytes(),
            JOURNAL_ALLOCATION_LIMITS,
            "protocol_version must be 0.32.0",
        );

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_batches = 1;
        assert_allocation_bound_error(&bytes, limits, "exceeds 1 batches");

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_proof_bytes = 31;
        assert_allocation_bound_error(&bytes, limits, "SNARK proof exceeds 44 encoded bytes");

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_operator_da_input_bytes = 31;
        assert_allocation_bound_error(&bytes, limits, "operator_da_input exceeds 31 bytes");

        let oversized = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: (MAX_JOURNALED_FRIS + 1) as u64,
            batches: marker_batches(1, (MAX_JOURNALED_FRIS + 1) as u64),
            proof: real_wrapper(),
        };
        assert_allocation_bound_error(
            &serialize_record(&oversized).unwrap(),
            JOURNAL_ALLOCATION_LIMITS,
            "exceeds 100 batches",
        );

        let singleton = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 1,
            batches: marker_batches(1, 1),
            proof: real_wrapper(),
        };
        assert_allocation_bound_error(
            &serialize_record(&singleton).unwrap(),
            JOURNAL_ALLOCATION_LIMITS,
            "minimum is 2",
        );
    }

    #[test]
    fn deserialization_probe_bounds_logs_messages_and_edge_da_bytes() {
        let mut batches = marker_batches(1, 2);
        batches[0]
            .batch
            .logs
            .push(zksync_os_contract_interface::models::L2Log {
                l2_shard_id: 0,
                is_service: true,
                tx_number_in_batch: 0,
                sender: Address::ZERO,
                key: B256::ZERO,
                value: B256::ZERO,
            });
        batches[0].batch.messages.push(vec![1, 2, 3, 4]);
        batches[0].batch.batch_info.commit_info.edge_da_refs_input = vec![1, 2];
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches,
            proof: real_wrapper(),
        };
        let bytes = serialize_record(&record).unwrap();

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_logs_per_batch = 0;
        assert_allocation_bound_error(&bytes, limits, "batch logs exceeds 0 items");

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_messages_per_batch = 0;
        assert_allocation_bound_error(&bytes, limits, "journal messages exceed 0 entries");

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_message_bytes_per_batch = 3;
        assert_allocation_bound_error(&bytes, limits, "journal message bytes exceed 3 per batch");

        let mut limits = JOURNAL_ALLOCATION_LIMITS;
        limits.max_edge_da_refs_input_bytes = 1;
        assert_allocation_bound_error(&bytes, limits, "edge_da_refs_input exceeds 1 bytes");

        // SYSCOIN: V2 is a fresh-chain format; do not let legacy serde defaults silently erase the
        // explicit empty/non-empty edge-DA opening that was present when the proof was journaled.
        let mut missing_edge_input = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        missing_edge_input["batches"][0]["batch"]["commit_batch_info"]
            .as_object_mut()
            .unwrap()
            .remove("edge_da_refs_input");
        assert_allocation_bound_error(
            &serde_json::to_vec(&missing_edge_input).unwrap(),
            JOURNAL_ALLOCATION_LIMITS,
            "missing field `edge_da_refs_input`",
        );
    }

    #[test]
    fn replay_ranges_allow_safe_gaps_but_reject_overlap_and_proved_coverage() {
        let contiguous = vec![
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 1,
                batch_to: 2,
                batches: marker_batches(1, 2),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(1, 2)),
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 3,
                batch_to: 4,
                batches: marker_batches(3, 4),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(3, 4)),
        ];
        validate_replay_ranges(0, &contiguous).unwrap();

        let gapped = vec![
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 101,
                batch_to: 200,
                batches: marker_batches(101, 200),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(101, 200)),
        ];
        validate_replay_ranges(0, &gapped).unwrap();

        let overlapping = vec![
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 1,
                batch_to: 3,
                batches: marker_batches(1, 3),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(1, 3)),
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 3,
                batch_to: 4,
                batches: marker_batches(3, 4),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(3, 4)),
        ];
        assert!(
            validate_replay_ranges(0, &overlapping)
                .unwrap_err()
                .to_string()
                .contains("overlap")
        );
        let duplicate_coverage = vec![
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 1,
                batch_to: 2,
                batches: marker_batches(1, 2),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(1, 2)),
            DurableSnarkRecord {
                format_version: JOURNAL_FORMAT_VERSION,
                batch_from: 1,
                batch_to: 2,
                batches: marker_batches(1, 2),
                proof: real_wrapper(),
            }
            .into_journaled(journal_key(1, 2)),
        ];
        assert!(validate_replay_ranges(0, &duplicate_coverage).is_err());
        assert!(validate_replay_ranges(2, &contiguous).is_err());
    }

    #[tokio::test]
    async fn second_lease_101_to_200_survives_crash_while_first_1_to_100_is_missing()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let first_lease = marker_batches(1, 100);
        let second_lease = marker_batches(101, 200);
        assert_eq!(
            (
                first_lease.first().unwrap().batch_number(),
                first_lease.last().unwrap().batch_number()
            ),
            (1, 100)
        );
        let second = journal.persist(second_lease, real_wrapper()).await?;
        let key = second.key.clone();
        drop(first_lease);
        drop(second);
        drop(confirmations);
        drop(journal);

        // SYSCOIN: Model the crash after the second exact lease was fsynced and acknowledged but
        // before the first lease completed. FRI storage will rebuild 1-100; journal replay owns
        // only 101-200 and therefore must not demand a contiguous journal frontier.
        let (reopened, _reopened_confirmations) = SnarkProofJournal::open(temp.path()).await?;
        assert_eq!(reopened.record_count().await, 1);
        let record = load_record(&temp.path().join("snark_journal").join(key.clone())).await?;
        let replay = vec![record.into_journaled(key)];
        assert_eq!(replay[0].batch_range(), (101, 200));
        validate_replay_ranges(0, &replay)?;
        Ok(())
    }

    #[test]
    fn recovery_binds_first_predecessor_to_canonical_committed_metadata() {
        let record = DurableSnarkRecord {
            format_version: JOURNAL_FORMAT_VERSION,
            batch_from: 1,
            batch_to: 2,
            batches: marker_batches(1, 2),
            proof: real_wrapper(),
        };
        let mut canonical = record.batches[0].batch.previous_stored_batch_info.clone();
        validate_canonical_predecessor(&record, &canonical).unwrap();

        canonical.state_commitment = B256::repeat_byte(0x99);
        assert!(
            validate_canonical_predecessor(&record, &canonical)
                .unwrap_err()
                .to_string()
                .contains("predecessor does not match canonical")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restart_rejects_non_owner_only_final_file() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new()?;
        let (journal, confirmations) = SnarkProofJournal::open(temp.path()).await?;
        let journaled = journal
            .persist(marker_batches(1, 2), real_wrapper())
            .await?;
        let path = temp.path().join("snark_journal").join(journaled.key);
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).await?;
        drop(confirmations);
        drop(journal);

        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("mode 0600"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_sparse_final_is_rejected_before_read_allocation() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let directory = temp.path().join("snark_journal");
        fs::create_dir_all(&directory).await?;
        let path = directory.join(journal_key(1, 2));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let file = options.open(&path).await?;
        file.set_len((MAX_JOURNAL_RECORD_BYTES as u64) + 1).await?;
        file.sync_all().await?;

        let error = SnarkProofJournal::open(temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("durable SNARK journal exceeds"));
        Ok(())
    }
}
