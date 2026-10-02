use super::*;
use alloy::{
    consensus::{SignableTransaction, TxEnvelope, TxLegacy, transaction::Recovered},
    network::EthereumWallet,
    primitives::{Signature, TxKind, U64},
    providers::ProviderBuilder,
    rpc::{
        json_rpc::ErrorPayload,
        types::{Block, Transaction},
    },
    sol_types::SolValue,
    transports::mock::Asserter,
};
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};

fn block(number: u64, hash: B256) -> Block {
    let mut value: Block = Block::default();
    value.header.inner.number = number;
    value.header.hash = hash;
    value
}

async fn provider(asserter: &Asserter) -> NodeProvider {
    asserter.push_success(&block(1, B256::ZERO).header);
    asserter.push_success(&block(1, B256::ZERO).header);
    asserter.push_failure(ErrorPayload::method_not_found());
    asserter.push_success(&"anvil/v1.0.0");
    NodeProvider::new(
        ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone()),
    )
    .await
    .unwrap()
}

async fn fixture(
    asserter: &Asserter,
    bootstrap: bool,
) -> (ServicePublication, Work, Handoff, Bytes, tempfile::TempDir) {
    let vector: Value = serde_json::from_str(include_str!(
        "../../../../../scripts/prover-service/vector.json"
    ))
    .unwrap();
    let mut sidecar: ProverServiceSidecarV1 =
        serde_json::from_value(vector["sidecar"].clone()).unwrap();
    let config = ServicePublicationConfig {
        gate: Address::repeat_byte(11),
        gate_code_hash: keccak256(b"gate"),
        coordinator: if bootstrap {
            Address::ZERO
        } else {
            Address::repeat_byte(12)
        },
        coordinator_code_hash: if bootstrap {
            B256::ZERO
        } else {
            keccak256(b"coordinator")
        },
        policy_hash: sidecar.accepted_package.policyHash,
        production_vk_hash: sidecar.accepted_package.vkHash,
        sequencer: sidecar.accepted_package.sequencer,
        ..Default::default()
    };
    if bootstrap {
        sidecar.candidate = None;
        sidecar.accepted_package.wrapper = Address::ZERO;
        sidecar.accepted_package.wrapperBeneficiary = Address::ZERO;
        sidecar.accepted_package.rosterRoot = B256::ZERO;
        sidecar.accepted_package.turn = 0;
        sidecar.candidate_proof.clear();
        sidecar.wrapper_signature = Bytes::new();
    }
    let accepted = &sidecar.accepted_package;
    let work = Work {
        schema_version: 1,
        execution_chain_id: accepted.chainId,
        settlement_chain_id: sidecar.batch_outputs[0].settlementChainId,
        chain_address: accepted.chainAddress,
        gate: config.gate,
        gate_code_hash: config.gate_code_hash,
        coordinator: config.coordinator,
        coordinator_code_hash: config.coordinator_code_hash,
        policy_hash: config.policy_hash,
        production_vk_hash: config.production_vk_hash,
        sequencer: config.sequencer,
        timelock: Address::repeat_byte(13),
        required_confirmations: 3,
        batch_from: accepted.batchFrom,
        batch_to: accepted.batchTo,
        proof_data: serde_json::from_value(vector["proof_data"].clone()).unwrap(),
        batch_outputs: sidecar.batch_outputs.clone(),
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let publication = ServicePublication::new(
        ServicePublicationConfig {
            directory: root.join("journal"),
            ..config
        },
        provider(asserter).await,
        ConfirmationPolicy::new(3),
        work.timelock,
    )
    .unwrap();
    let calldata = Bytes::from_static(b"exact retained native calldata");
    let handoff = Handoff {
        schema_version: 1,
        work_hash: keccak256(serde_json::to_vec(&work).unwrap()),
        transaction_hash: B256::ZERO,
        sidecar,
    };
    (publication, work, handoff, calldata, temporary)
}

fn transaction(work: &Work, calldata: Bytes) -> Transaction {
    let tx = TxLegacy {
        chain_id: Some(work.settlement_chain_id.to::<u64>()),
        to: TxKind::Call(work.gate),
        input: calldata,
        gas_limit: 1_000_000,
        gas_price: 1,
        ..Default::default()
    };
    let envelope: TxEnvelope = tx
        .into_signed(Signature::new(U256::from(1), U256::from(2), false))
        .into();
    Transaction {
        inner: Recovered::new_unchecked(envelope, Address::repeat_byte(14)),
        block_hash: Some(B256::repeat_byte(42)),
        block_number: Some(42),
        transaction_index: Some(0),
        effective_gas_price: Some(1),
        block_timestamp: None,
    }
}

fn receipt(work: &Work, handoff: &Handoff) -> Value {
    let event = IZkSysProofGateV1::ServicePackageAccepted {
        packageHash: hash_package(&handoff.sidecar.accepted_package),
        from: work.batch_from,
        to: work.batch_to,
        bootstrap: handoff.sidecar.candidate.is_none(),
    };
    let log = event.encode_log_data();
    json!({ "type":"0x0", "status":"0x1", "cumulativeGasUsed":"0x1", "gasUsed":"0x1",
        "effectiveGasPrice":"0x1", "transactionHash":handoff.transaction_hash,
        "transactionIndex":"0x0", "blockHash":B256::repeat_byte(42), "blockNumber":"0x2a",
        "from":Address::repeat_byte(14), "to":work.gate, "contractAddress":null,
        "logsBloom":format!("0x{}", "00".repeat(256)),
        "logs":[{"address":work.gate, "topics":log.topics(), "data":log.data,
            "blockHash":B256::repeat_byte(42), "blockNumber":"0x2a", "transactionHash":handoff.transaction_hash,
            "transactionIndex":"0x0", "logIndex":"0x0", "removed":false}] })
}

fn push_start(
    asserter: &Asserter,
    work: &Work,
    handoff: &Handoff,
    transaction: &Transaction,
    receipt: &Value,
    tip: u64,
) {
    assert_eq!(handoff.transaction_hash, transaction.tx_hash());
    asserter.push_success(&U64::from(work.settlement_chain_id.to::<u64>()));
    asserter.push_success(receipt);
    asserter.push_success(&block(tip, B256::repeat_byte(tip as u8)));
    if tip >= 44 {
        asserter.push_success(transaction);
    }
}

fn push_contracts(asserter: &Asserter, work: &Work) {
    asserter.push_success(&Bytes::from_static(b"gate"));
    for encoded in [
        work.chain_address.abi_encode(),
        work.execution_chain_id.abi_encode(),
        work.timelock.abi_encode(),
        work.policy_hash.abi_encode(),
        work.production_vk_hash.abi_encode(),
        work.sequencer.abi_encode(),
    ] {
        asserter.push_success(&Bytes::from(encoded));
    }
    if !work.coordinator.is_zero() {
        asserter.push_success(&Bytes::from(work.coordinator.abi_encode()));
        asserter.push_success(&Bytes::from_static(b"coordinator"));
        for encoded in [
            work.gate.abi_encode(),
            work.execution_chain_id.abi_encode(),
            work.chain_address.abi_encode(),
            work.policy_hash.abi_encode(),
            work.production_vk_hash.abi_encode(),
            work.sequencer.abi_encode(),
        ] {
            asserter.push_success(&Bytes::from(encoded));
        }
    }
}

#[tokio::test]
async fn confirms_exact_bootstrap_and_service_receipts_at_inclusive_depth() {
    for bootstrap in [true, false] {
        let asserter = Asserter::new();
        let (publication, work, mut handoff, calldata, _temp) = fixture(&asserter, bootstrap).await;
        let tx = transaction(&work, calldata.clone());
        handoff.transaction_hash = tx.tx_hash();
        let receipt = receipt(&work, &handoff);
        push_start(&asserter, &work, &handoff, &tx, &receipt, 44);
        push_contracts(&asserter, &work);
        asserter.push_success(&block(42, B256::repeat_byte(42)));
        asserter.push_success(&block(44, B256::repeat_byte(44)));
        asserter.push_success(&receipt);
        asserter.push_success(&U64::from(work.settlement_chain_id.to::<u64>()));
        let confirmed = publication
            .observe(&work, handoff.work_hash, &handoff, &calldata)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(confirmed.transaction_hash, handoff.transaction_hash);
        assert_eq!(confirmed.calldata_hash, keccak256(calldata));
    }
}

#[tokio::test]
async fn insufficient_depth_preserves_pending_work() {
    let asserter = Asserter::new();
    let (publication, work, mut handoff, calldata, _temp) = fixture(&asserter, true).await;
    let tx = transaction(&work, calldata.clone());
    handoff.transaction_hash = tx.tx_hash();
    push_start(
        &asserter,
        &work,
        &handoff,
        &tx,
        &receipt(&work, &handoff),
        43,
    );
    assert!(
        publication
            .observe(&work, handoff.work_hash, &handoff, &calldata)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn rejects_foreign_calldata_missing_event_reverted_receipt_and_wrong_runtime() {
    for mutation in ["calldata", "event", "reverted", "runtime"] {
        let asserter = Asserter::new();
        let (publication, work, mut handoff, calldata, _temp) = fixture(&asserter, true).await;
        let tx = transaction(
            &work,
            if mutation == "calldata" {
                Bytes::from_static(b"other proof")
            } else {
                calldata.clone()
            },
        );
        handoff.transaction_hash = tx.tx_hash();
        let mut receipt = receipt(&work, &handoff);
        if mutation == "event" {
            receipt["logs"] = json!([]);
        }
        if mutation == "reverted" {
            receipt["status"] = json!("0x0");
        }
        push_start(&asserter, &work, &handoff, &tx, &receipt, 44);
        if mutation == "runtime" {
            asserter.push_success(&Bytes::from_static(b"unreviewed"));
        }
        assert!(
            publication
                .observe(&work, handoff.work_hash, &handoff, &calldata)
                .await
                .is_err(),
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn rejects_reorg_and_receipt_disappearance_after_historical_checks() {
    for mutation in ["block", "tip", "receipt"] {
        let asserter = Asserter::new();
        let (publication, work, mut handoff, calldata, _temp) = fixture(&asserter, true).await;
        let tx = transaction(&work, calldata.clone());
        handoff.transaction_hash = tx.tx_hash();
        let receipt = receipt(&work, &handoff);
        push_start(&asserter, &work, &handoff, &tx, &receipt, 44);
        push_contracts(&asserter, &work);
        asserter.push_success(&block(
            42,
            B256::repeat_byte(if mutation == "block" { 99 } else { 42 }),
        ));
        asserter.push_success(&block(
            44,
            B256::repeat_byte(if mutation == "tip" { 99 } else { 44 }),
        ));
        if mutation == "receipt" {
            asserter.push_success(&Value::Null);
        } else {
            asserter.push_success(&receipt);
        }
        asserter.push_success(&U64::from(work.settlement_chain_id.to::<u64>()));
        assert!(
            publication
                .observe(&work, handoff.work_hash, &handoff, &calldata)
                .await
                .is_err(),
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn journal_restart_freezes_native_work_and_pins_and_enforces_capacity() {
    let asserter = Asserter::new();
    let (_, mut work, _, _, _temp) = fixture(&asserter, true).await;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().canonicalize().unwrap().join("publication");
    let journal = Journal::open(&path).unwrap();
    let (_, directory, hash) = journal.stage(&work, 1).unwrap();
    assert!(Journal::open(&path).is_err());
    drop(journal);
    let restarted = Journal::open(&path).unwrap();
    assert_eq!(
        restarted.stage(&work, 1).unwrap(),
        (work.clone(), directory.clone(), hash)
    );
    work.gate_code_hash = B256::repeat_byte(90);
    assert!(restarted.stage(&work, 1).is_err());
    work.batch_to += 1;
    assert!(restarted.stage(&work, 1).is_err());
    fs::set_permissions(
        directory.join("work.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(read_private(&directory.join("work.json")).is_err());
}

#[test]
fn handoff_rejects_symlinks_hardlinks_and_unknown_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().canonicalize().unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let file = path.join("original");
    atomic_write(&file, b"{}").unwrap();
    symlink(&file, path.join("symlink")).unwrap();
    assert!(read_private(&path.join("symlink")).is_err());
    fs::hard_link(&file, path.join("hardlink")).unwrap();
    assert!(read_private(&file).is_err());
    symlink(&path, path.join("directory-link")).unwrap();
    assert!(private_directory(&path.join("directory-link/subdir")).is_err());
    assert!(
        serde_json::from_value::<Handoff>(json!({"schema_version":1, "verified":true})).is_err()
    );
}

#[tokio::test]
async fn covered_native_restart_waits_for_receipt_and_preserves_bootstrap_work_on_activation() {
    use crate::prover_api::{
        snark_proof_journal::SnarkProofJournal, test_util::create_test_batch_envelope_with_data,
    };
    use zksync_os_batch_types::batcher_model::{BatchSignatureData, RealSnarkProof, SnarkProof};
    use zksync_os_types::ProtocolSemanticVersion;

    let asserter = Asserter::new();
    let (mut publication, old_work, mut handoff, _, temporary) = fixture(&asserter, true).await;
    let mut batches = Vec::new();
    let mut previous = None;
    for number in 1..=2 {
        let mut batch = create_test_batch_envelope_with_data(
            number,
            ProtocolSemanticVersion::new(0, 32, 0),
            FriProof::AlreadySubmittedToL1,
        );
        batch.batch.chain_address = old_work.chain_address;
        batch.batch.batch_info.commit_info.chain_id = old_work.execution_chain_id.to::<u64>();
        batch.batch.batch_info.commit_info.sl_chain_id = old_work.settlement_chain_id.to::<u64>();
        batch.signature_data = BatchSignatureData::AlreadyCommitted;
        if let Some(previous) = previous {
            batch.batch.previous_stored_batch_info = previous
        }
        previous = Some(batch.batch.batch_info.clone().into_stored());
        batches.push(batch);
    }
    let (native, _confirmations) = SnarkProofJournal::open(temporary.path()).await.unwrap();
    native
        .persist(
            batches,
            SnarkProof::Real(RealSnarkProof {
                proof: vec![7; 1408],
                proving_execution_version: 8,
            }),
        )
        .await
        .unwrap();
    assert!(native.covered_service_commands(1).await.unwrap().is_empty());
    let covered = native.covered_service_commands(2).await.unwrap();
    assert_eq!(covered.len(), 1);
    let command = &covered[0];
    let work = Work::new(command, &publication).unwrap();
    let (_, directory, work_hash) = publication.journal.stage(&work, 256).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            publication.confirm_command(command)
        )
        .await
        .is_err()
    );
    assert_eq!(native.record_count().await, 1);
    assert!(!directory.join("confirmed.json").exists());

    handoff.sidecar.accepted_package.proofHash = keccak256(&work.proof_data);
    handoff.sidecar.accepted_package.reportHash =
        zksync_os_contract_interface::prover_service_v1::hash_report(&[]);
    handoff.sidecar.duties.clear();
    handoff.sidecar.batch_outputs = work.batch_outputs.clone();
    handoff.work_hash = work_hash;
    let calldata = command
        .service_submission(
            work.gate,
            &handoff.sidecar,
            &work.context(&handoff.sidecar).unwrap(),
        )
        .unwrap()
        .calldata;
    assert_eq!(
        calldata,
        encode_historical_calldata(&work, &handoff.sidecar)
    );
    let tx = transaction(&work, calldata);
    handoff.transaction_hash = tx.tx_hash();
    atomic_write(
        &directory.join("relay.json"),
        &serde_json::to_vec(&handoff).unwrap(),
    )
    .unwrap();
    let receipt = receipt(&work, &handoff);
    push_start(&asserter, &work, &handoff, &tx, &receipt, 44);
    push_contracts(&asserter, &work);
    asserter.push_success(&block(42, B256::repeat_byte(42)));
    asserter.push_success(&block(44, B256::repeat_byte(44)));
    asserter.push_success(&receipt);
    asserter.push_success(&U64::from(work.settlement_chain_id.to::<u64>()));

    // Installation may follow the accepted bootstrap in the same block. Recovery preserves the
    // exact old zero-coordinator work while the configured next service lane pins its coordinator.
    publication.config.coordinator = Address::repeat_byte(88);
    publication.config.coordinator_code_hash = B256::repeat_byte(89);
    publication.confirm_command(command).await.unwrap();
    assert_eq!(native.record_count().await, 1);
    assert!(directory.join("confirmed.json").exists());
    let bytes = read_private(&directory.join("work.json")).unwrap().unwrap();
    assert_eq!(keccak256(bytes), work_hash);
    assert_eq!(
        publication
            .journal
            .work_for_batch(&command.as_ref()[0], &publication)
            .unwrap(),
        work
    );
    let mut wrong = create_test_batch_envelope_with_data(
        1,
        ProtocolSemanticVersion::new(0, 32, 0),
        FriProof::AlreadySubmittedToL1,
    );
    wrong.batch = command.as_ref()[0].batch.clone();
    wrong.batch.batch_info.commit_info.sl_chain_id += 1;
    assert!(
        publication
            .journal
            .work_for_batch(&wrong, &publication)
            .is_err()
    );
    let fake = ProofCommand::new(vec![wrong], SnarkProof::Fake);
    assert!(Work::new(&fake, &publication).is_err());
}

#[tokio::test]
async fn restart_cannot_lower_the_frozen_confirmation_depth() {
    for (current, frozen) in [(1, 3), (3, 1)] {
        let asserter = Asserter::new();
        let (mut publication, mut work, mut handoff, calldata, _temp) =
            fixture(&asserter, true).await;
        publication.confirmation_policy = ConfirmationPolicy::new(current);
        work.required_confirmations = frozen;
        let tx = transaction(&work, calldata.clone());
        handoff.transaction_hash = tx.tx_hash();
        push_start(
            &asserter,
            &work,
            &handoff,
            &tx,
            &receipt(&work, &handoff),
            43,
        );
        assert!(
            publication
                .observe(&work, handoff.work_hash, &handoff, &calldata)
                .await
                .unwrap()
                .is_none()
        );
    }
}
