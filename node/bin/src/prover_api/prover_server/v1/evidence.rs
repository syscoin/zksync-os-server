//! Read-only producer metadata. Wrappers must compare this evidence with independent execution.

use std::{future::Future, sync::Arc};

use alloy::{
    primitives::{Address, B256, U256, keccak256},
    sol_types::SolValue,
};
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use http::{HeaderValue, StatusCode, header::CACHE_CONTROL};
use serde::Serialize;
use tokio::sync::Semaphore;
use zksync_os_batch_types::batcher_model::{BatchMetadata, FriProof, SignedBatchEnvelope};
use zksync_os_contract_interface::{
    IExecutor,
    prover_service_v1::{
        BatchOutput, MAX_BATCHES_PER_PACKAGE, PROTOCOL_VERSION, StoredBatch, batch_output_hash,
    },
};
use zksync_os_types::{ProtocolSemanticVersion, ProvingVersion};

use super::handlers::{retain_response_slot, try_acquire_response_slot};
use crate::prover_api::prover_server::AppState;

#[derive(Debug, Serialize)]
struct ProducerEvidenceV1 {
    schema_version: u32,
    chain_id: U256,
    chain_address: Address,
    settlement_chain_id: U256,
    protocol_version: u32,
    vk_hash: B256,
    previous_batch: StoredBatch,
    batches: Vec<BatchEvidenceV1>,
}

#[derive(Clone, Debug, Serialize)]
struct BatchEvidenceV1 {
    stored: StoredBatch,
    output: BatchOutput,
}

#[derive(Clone, Debug)]
struct EvidenceRecord {
    chain_id: U256,
    chain_address: Address,
    settlement_chain_id: U256,
    vk_hash: B256,
    previous: StoredBatch,
    batch: BatchEvidenceV1,
}

#[derive(Debug, thiserror::Error)]
enum EvidenceError {
    #[error(
        "evidence requires an ordered nonzero range of at most 100 batches within the configured limit"
    )]
    Range,
    #[error("no retained FRI batch found for {0}")]
    Missing(u64),
    #[error("failed to read retained FRI batch {0}")]
    Storage(u64),
    #[error("invalid retained FRI batch {0}: {1}")]
    Invalid(u64, &'static str),
    #[error("canonical V8 verification key regeneration is required")]
    VkUnavailable,
}

impl IntoResponse for EvidenceError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Range => StatusCode::BAD_REQUEST,
            Self::Missing(_) => StatusCode::NOT_FOUND,
            Self::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Invalid(..) | Self::VkUnavailable => StatusCode::CONFLICT,
        };
        (status, self.to_string()).into_response()
    }
}

fn batch_count(from: u64, to: u64, configured_maximum: usize) -> Result<usize, EvidenceError> {
    let count = to
        .checked_sub(from)
        .and_then(|distance| distance.checked_add(1));
    match count {
        Some(count)
            if from != 0
                && count <= MAX_BATCHES_PER_PACKAGE
                && count <= configured_maximum as u64 =>
        {
            Ok(count as usize)
        }
        _ => Err(EvidenceError::Range),
    }
}

impl EvidenceRecord {
    fn from_retained(
        number: u64,
        envelope: SignedBatchEnvelope<FriProof>,
    ) -> Result<Self, EvidenceError> {
        match &envelope.data {
            FriProof::Real(proof)
                if proof.proving_execution_version == ProvingVersion::V8 as u32
                    && !proof.proof().is_empty() => {}
            FriProof::Real(_) => {
                return Err(EvidenceError::Invalid(number, "missing real V8 proof"));
            }
            FriProof::Fake => return Err(EvidenceError::Invalid(number, "fake proof")),
            FriProof::AlreadySubmittedToL1 => {
                return Err(EvidenceError::Invalid(
                    number,
                    "already submitted to settlement",
                ));
            }
        }
        Self::from_pending(number, &envelope.batch)
    }

    fn from_pending(number: u64, metadata: &BatchMetadata) -> Result<Self, EvidenceError> {
        let version = metadata
            .proving_version()
            .map_err(|_| EvidenceError::Invalid(number, "unsupported proving version"))?;
        if version != ProvingVersion::V8 || version.requires_vk_regeneration() {
            return Err(EvidenceError::VkUnavailable);
        }
        let vk_hash: B256 = version
            .vk_hash()
            .parse()
            .map_err(|_| EvidenceError::VkUnavailable)?;
        if vk_hash.is_zero() {
            return Err(EvidenceError::VkUnavailable);
        }
        Self::from_metadata(number, metadata, vk_hash)
    }

    fn from_metadata(
        number: u64,
        metadata: &BatchMetadata,
        vk_hash: B256,
    ) -> Result<Self, EvidenceError> {
        let batch = &metadata.batch_info;
        let commit = &batch.commit_info;
        if batch.protocol_version != ProtocolSemanticVersion::new(0, 32, 0)
            || commit.batch_number != number
            || number == 0
            || metadata
                .previous_stored_batch_info
                .batch_number
                .checked_add(1)
                != Some(number)
        {
            return Err(EvidenceError::Invalid(
                number,
                "nonconsecutive batch or unsupported protocol",
            ));
        }
        if commit.chain_id == 0
            || commit.sl_chain_id == 0
            || metadata.chain_address.is_zero()
            || vk_hash.is_zero()
        {
            return Err(EvidenceError::Invalid(number, "zero chain identity or VK"));
        }
        if commit.first_block_timestamp > commit.last_block_timestamp
            || commit
                .number_of_layer1_txs
                .checked_add(commit.number_of_layer2_txs)
                .is_none()
        {
            return Err(EvidenceError::Invalid(
                number,
                "invalid native timestamps or transaction count",
            ));
        }
        let output = BatchOutput {
            firstBlockTimestamp: commit.first_block_timestamp,
            lastBlockTimestamp: commit.last_block_timestamp,
            daScheme: U256::from(commit.l2_da_commitment_scheme as u8),
            daCommitment: commit.da_commitment,
            l1TxCount: U256::from(commit.number_of_layer1_txs),
            l2TxCount: U256::from(commit.number_of_layer2_txs),
            priorityOperationsHash: commit.priority_operations_hash,
            l2LogsRoot: commit.l2_to_l1_logs_root_hash,
            upgradeTxHash: batch.upgrade_tx_hash.unwrap_or(B256::ZERO),
            dependencyRootsRollingHash: commit.dependency_roots_rolling_hash,
            settlementChainId: U256::from(commit.sl_chain_id),
            edgeDARefsRoot: commit.edge_da_refs_root,
        };
        let stored = StoredBatch {
            batchNumber: number,
            batchHash: commit.new_state_commitment,
            indexRepeatedStorageChanges: 0,
            numberOfLayer1Txs: U256::from(commit.number_of_layer1_txs),
            priorityOperationsHash: commit.priority_operations_hash,
            dependencyRootsRollingHash: commit.dependency_roots_rolling_hash,
            l2LogsTreeRoot: commit.l2_to_l1_logs_root_hash,
            timestamp: U256::ZERO,
            commitment: batch.batch_output_hash(),
        };
        if stored.commitment != batch_output_hash(&output) {
            return Err(EvidenceError::Invalid(
                number,
                "native output encoding mismatch",
            ));
        }
        // Reuse the native ABI conversion so obsolete metadata timestamps never alter stored hashes.
        let previous = IExecutor::StoredBatchInfo::from(&metadata.previous_stored_batch_info);
        Ok(Self {
            chain_id: U256::from(commit.chain_id),
            chain_address: metadata.chain_address,
            settlement_chain_id: U256::from(commit.sl_chain_id),
            vk_hash,
            previous: StoredBatch {
                batchNumber: previous.batchNumber,
                batchHash: previous.batchHash,
                indexRepeatedStorageChanges: previous.indexRepeatedStorageChanges,
                numberOfLayer1Txs: previous.numberOfLayer1Txs,
                priorityOperationsHash: previous.priorityOperationsHash,
                dependencyRootsRollingHash: previous.dependencyRootsRollingHash,
                l2LogsTreeRoot: previous.l2LogsTreeRoot,
                timestamp: previous.timestamp,
                commitment: previous.commitment,
            },
            batch: BatchEvidenceV1 { stored, output },
        })
    }
}

impl ProducerEvidenceV1 {
    fn append(&mut self, record: EvidenceRecord) -> Result<(), EvidenceError> {
        let number = record.batch.stored.batchNumber;
        if record.chain_id != self.chain_id
            || record.chain_address != self.chain_address
            || record.settlement_chain_id != self.settlement_chain_id
            || record.vk_hash != self.vk_hash
        {
            return Err(EvidenceError::Invalid(
                number,
                "mixed deployment identity or VK",
            ));
        }
        let previous = self
            .batches
            .last()
            .map_or(&self.previous_batch, |batch| &batch.stored);
        if previous.batchNumber.checked_add(1) != Some(number)
            || keccak256(previous.abi_encode()) != keccak256(record.previous.abi_encode())
        {
            return Err(EvidenceError::Invalid(
                number,
                "previous stored batch hash mismatch",
            ));
        }
        self.batches.push(record.batch);
        Ok(())
    }
}

async fn evidence_response<F, Fut>(
    from: u64,
    to: u64,
    maximum: usize,
    slots: &Arc<Semaphore>,
    mut read: F,
) -> Response
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<Option<EvidenceRecord>, EvidenceError>>,
{
    let count = match batch_count(from, to, maximum) {
        Ok(count) => count,
        Err(error) => return error.into_response(),
    };
    // Stored records include large FRI byte arrays even though this response never publishes them.
    // Sharing admission with SNARK peek bounds disk reads and deserialization before either starts.
    let Some(permit) = try_acquire_response_slot(slots) else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "SNARK peek capacity is busy; retry later",
        )
            .into_response();
    };
    let mut evidence: Option<ProducerEvidenceV1> = None;
    for number in from..=to {
        let record = match read(number).await {
            Ok(Some(record)) => record,
            Ok(None) => return EvidenceError::Missing(number).into_response(),
            Err(error) => return error.into_response(),
        };
        let evidence = evidence.get_or_insert_with(|| ProducerEvidenceV1 {
            schema_version: 1,
            chain_id: record.chain_id,
            chain_address: record.chain_address,
            settlement_chain_id: record.settlement_chain_id,
            protocol_version: PROTOCOL_VERSION,
            vk_hash: record.vk_hash,
            previous_batch: record.previous.clone(),
            batches: Vec::with_capacity(count),
        });
        if let Err(error) = evidence.append(record) {
            return error.into_response();
        }
    }
    let Some(evidence) = evidence else {
        return EvidenceError::Range.into_response();
    };
    let mut response = Json(evidence).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    retain_response_slot(response, permit)
}

pub(super) async fn snark_evidence(
    Path((from, to)): Path<(u64, u64)>,
    State(state): State<AppState>,
) -> Response {
    let storage = &state.proof_storage;
    evidence_response(
        from,
        to,
        state.max_fris_per_snark,
        &state.snark_peek_slots,
        |number| async move {
            let envelope = storage
                .get_batch_with_proof(number)
                .await
                .map_err(|error| {
                    tracing::warn!(?error, number, "failed to read producer evidence");
                    EvidenceError::Storage(number)
                })?;
            envelope
                .map(|envelope| EvidenceRecord::from_retained(number, envelope))
                .transpose()
        },
    )
    .await
}

/// Pending metadata authenticates a work statement, never successful proof production.
pub(super) async fn fri_evidence(
    Path(number): Path<u64>,
    State(state): State<AppState>,
) -> Response {
    let manager = &state.fri_job_manager;
    evidence_response(
        number,
        number,
        1,
        &state.fri_peek_slots,
        |number| async move {
            manager
                .pending_batch_metadata(number)
                .await
                .map(|metadata| EvidenceRecord::from_pending(number, &metadata))
                .transpose()
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, body::Body, extract::Request, routing::get};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    use zksync_os_batch_types::batcher_model::RealFriProof;

    use crate::prover_api::test_util::create_test_batch_envelope_with_data;

    fn metadata(number: u64) -> BatchMetadata {
        let mut batch = create_test_batch_envelope_with_data(
            number,
            ProtocolSemanticVersion::new(0, 32, 0),
            (),
        )
        .batch;
        batch.chain_address = Address::repeat_byte(0x11);
        batch.batch_info.number_of_layer1_txs = 2;
        batch.batch_info.number_of_layer2_txs = 3;
        batch.batch_info.first_block_timestamp = 101;
        batch.batch_info.last_block_timestamp = 110;
        batch.batch_info.new_state_commitment = B256::with_last_byte(number as u8);
        batch.batch_info.upgrade_tx_hash = Some(B256::repeat_byte(0x44));
        batch.batch_info.edge_da_refs_root = B256::repeat_byte(0x55);
        batch
    }

    fn records() -> Vec<EvidenceRecord> {
        let first = metadata(1);
        let mut second = metadata(2);
        second.previous_stored_batch_info = first.batch_info.clone().into_stored();
        [first, second]
            .iter()
            .map(|batch| {
                EvidenceRecord::from_metadata(
                    batch.batch_info.batch_number,
                    batch,
                    B256::repeat_byte(0x33),
                )
                .unwrap()
            })
            .collect()
    }

    fn router(
        records: Vec<EvidenceRecord>,
        slots: Arc<Semaphore>,
        reads: Arc<AtomicUsize>,
    ) -> Router {
        let records = Arc::new(records);
        Router::new().route(
            "/SNARK/{from}/{to}/evidence",
            get(move |Path((from, to)): Path<(u64, u64)>| {
                let records = records.clone();
                let slots = slots.clone();
                let reads = reads.clone();
                async move {
                    evidence_response(from, to, 100, &slots, |number| {
                        reads.fetch_add(1, Ordering::SeqCst);
                        std::future::ready(Ok(records
                            .iter()
                            .find(|record| record.batch.stored.batchNumber == number)
                            .cloned()))
                    })
                    .await
                }
            }),
        )
    }

    fn request(path: &str) -> Request {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    #[test]
    fn range_bounds_precede_storage_work() {
        assert_eq!(batch_count(1, 1, 100).unwrap(), 1);
        assert_eq!(batch_count(1, 100, 100).unwrap(), 100);
        assert_eq!(batch_count(u64::MAX, u64::MAX, 100).unwrap(), 1);
        for (from, to, maximum) in [
            (0, 0, 100),
            (0, u64::MAX, 100),
            (2, 1, 100),
            (1, 101, 100),
            (1, 2, 1),
            (1, 1, 0),
        ] {
            assert!(batch_count(from, to, maximum).is_err());
        }
    }

    #[test]
    fn projection_preserves_canonical_native_output_and_stored_abi() {
        let mut batch = metadata(1);
        batch.previous_stored_batch_info.last_block_timestamp = Some(12345);
        let record = EvidenceRecord::from_metadata(1, &batch, B256::repeat_byte(0x33)).unwrap();
        assert_eq!(record.previous.timestamp, U256::ZERO);
        assert_eq!(record.batch.output.upgradeTxHash, B256::repeat_byte(0x44));
        assert_eq!(record.batch.output.edgeDARefsRoot, B256::repeat_byte(0x55));
        assert_eq!(
            record.batch.output.l1TxCount + record.batch.output.l2TxCount,
            U256::from(5)
        );
        assert_eq!(
            record.batch.stored.commitment,
            batch.batch_info.batch_output_hash()
        );
        assert_eq!(
            record.batch.stored.abi_encode(),
            IExecutor::StoredBatchInfo::from(&batch.batch_info.clone().into_stored()).abi_encode()
        );
        assert_eq!(
            keccak256(record.previous.abi_encode()),
            batch.previous_stored_batch_info.hash()
        );
    }

    #[test]
    fn retained_markers_fake_proofs_and_unavailable_key_never_export() {
        for proof in [
            FriProof::Fake,
            FriProof::AlreadySubmittedToL1,
            FriProof::Real(RealFriProof {
                proof: vec![1, 2, 3].into(),
                proving_execution_version: 7,
            }),
            FriProof::Real(RealFriProof {
                proof: vec![].into(),
                proving_execution_version: 8,
            }),
        ] {
            let envelope = create_test_batch_envelope_with_data(
                1,
                ProtocolSemanticVersion::new(0, 32, 0),
                proof,
            );
            assert!(matches!(
                EvidenceRecord::from_retained(1, envelope),
                Err(EvidenceError::Invalid(..))
            ));
        }
        if ProvingVersion::V8.requires_vk_regeneration()
            || ProvingVersion::V8
                .vk_hash()
                .parse::<B256>()
                .unwrap()
                .is_zero()
        {
            let mut envelope = create_test_batch_envelope_with_data(
                1,
                ProtocolSemanticVersion::new(0, 32, 0),
                FriProof::Real(RealFriProof {
                    proof: vec![1, 2, 3].into(),
                    proving_execution_version: 8,
                }),
            );
            envelope.batch = metadata(1);
            assert!(matches!(
                EvidenceRecord::from_retained(1, envelope),
                Err(EvidenceError::VkUnavailable)
            ));
        }
        assert!(EvidenceRecord::from_metadata(1, &metadata(1), B256::ZERO).is_err());
    }

    #[test]
    fn malformed_metadata_is_rejected_before_projection() {
        let mut variants = vec![metadata(1); 7];
        variants[0].batch_info.protocol_version = ProtocolSemanticVersion::new(0, 32, 1);
        variants[1].batch_info.chain_id = 0;
        variants[2].batch_info.sl_chain_id = 0;
        variants[3].chain_address = Address::ZERO;
        variants[4].previous_stored_batch_info.batch_number = 1;
        variants[5].batch_info.first_block_timestamp = 111;
        variants[6].batch_info.number_of_layer1_txs = u64::MAX;
        for batch in variants {
            assert!(EvidenceRecord::from_metadata(1, &batch, B256::repeat_byte(0x33)).is_err());
        }
    }

    #[tokio::test]
    async fn mixed_identity_and_broken_previous_hash_fail_the_whole_range() {
        let original = records();
        let mut changed = vec![original[1].clone(); 5];
        changed[0].chain_id += U256::from(1);
        changed[1].settlement_chain_id += U256::from(1);
        changed[2].chain_address = Address::repeat_byte(0x99);
        changed[3].vk_hash = B256::repeat_byte(0x99);
        changed[4].previous.commitment = B256::repeat_byte(0x99);
        for second in changed {
            let app = router(
                vec![original[0].clone(), second],
                Arc::new(Semaphore::new(1)),
                Arc::new(AtomicUsize::new(0)),
            );
            let response = app.oneshot(request("/SNARK/1/2/evidence")).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(
                !String::from_utf8(body.to_vec())
                    .unwrap()
                    .contains("\"batches\"")
            );
        }
    }

    #[tokio::test]
    async fn invalid_ranges_and_busy_peek_lane_perform_no_reads() {
        let slots = Arc::new(Semaphore::new(1));
        let reads = Arc::new(AtomicUsize::new(0));
        let app = router(records(), slots.clone(), reads.clone());
        for path in [
            "/SNARK/0/1/evidence",
            "/SNARK/1/101/evidence",
            "/SNARK/2/1/evidence",
        ] {
            let response = app.clone().oneshot(request(path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let held = slots.clone().acquire_owned().await.unwrap();
        let response = app.oneshot(request("/SNARK/1/2/evidence")).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        drop(held);
    }

    #[tokio::test]
    async fn response_schema_excludes_proofs_and_holds_shared_slot_through_drain() {
        let slots = Arc::new(Semaphore::new(1));
        let reads = Arc::new(AtomicUsize::new(0));
        let app = router(records(), slots.clone(), reads.clone());
        let response = app
            .clone()
            .oneshot(request("/SNARK/1/2/evidence"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(slots.available_permits(), 0);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let blocked = app.oneshot(request("/SNARK/1/2/evidence")).await.unwrap();
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(slots.available_permits(), 1);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let expected = [
            "schema_version",
            "chain_id",
            "chain_address",
            "settlement_chain_id",
            "protocol_version",
            "vk_hash",
            "previous_batch",
            "batches",
        ];
        assert_eq!(json.as_object().unwrap().len(), expected.len());
        for field in expected {
            assert!(json.get(field).is_some(), "missing {field}");
        }
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["protocol_version"], 32);
        assert_eq!(json["chain_id"], "0x1");
        assert_eq!(json["settlement_chain_id"], "0x2");
        assert_eq!(json["previous_batch"]["batchNumber"], 0);
        assert_eq!(json["batches"].as_array().unwrap().len(), 2);
        assert_eq!(json["batches"][0].as_object().unwrap().len(), 2);
        assert!(json["batches"][0].get("stored").is_some());
        assert!(json["batches"][0].get("output").is_some());
    }

    #[tokio::test]
    async fn missing_batch_never_returns_a_partial_success() {
        let slots = Arc::new(Semaphore::new(1));
        let reads = Arc::new(AtomicUsize::new(0));
        let app = router(vec![records().remove(0)], slots.clone(), reads.clone());
        let response = app.oneshot(request("/SNARK/1/2/evidence")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert_eq!(slots.available_permits(), 1);
    }
}
