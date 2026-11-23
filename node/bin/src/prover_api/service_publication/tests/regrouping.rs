use super::*;
use zksync_os_batch_types::batcher_model::{RealSnarkProof, SnarkProof};

fn command(work: &Work, from: u64, to: u64, generation: u8) -> ProofCommand {
    ProofCommand::new(
        publication_batches(work, from, to),
        SnarkProof::Real(RealSnarkProof {
            proof: vec![generation; 1408],
            proving_execution_version: 8,
        }),
    )
}

fn snapshot(asserter: &Asserter, work: &Work, latest: u64, safe: u64) {
    let chain_id = U64::from(work.settlement_chain_id.to::<u64>());
    let tip = block(5, B256::repeat_byte(5));
    let confirmed = block(3, B256::repeat_byte(3));
    asserter.push_success(&chain_id);
    asserter.push_success(&tip);
    asserter.push_success(&confirmed);
    asserter.push_success(&Bytes::from(latest.abi_encode()));
    asserter.push_success(&Bytes::from(safe.abi_encode()));
    asserter.push_success(&tip);
    asserter.push_success(&confirmed);
    asserter.push_success(&chain_id);
}

fn anchor(asserter: &Asserter, work: &Work) {
    anchor_at(asserter, work, 2, 2, B256::repeat_byte(2));
}

fn anchor_at(asserter: &Asserter, work: &Work, latest: u64, safe: u64, batch_hash: B256) {
    snapshot(asserter, work, latest, safe);
    asserter.push_success(&Bytes::from_static(b"gate"));
    asserter.push_success(&Bytes::from(work.chain_address.abi_encode()));
    asserter.push_success(&Bytes::from(work.execution_chain_id.abi_encode()));
    asserter.push_success(&Bytes::from(batch_hash.abi_encode()));
}

#[tokio::test]
async fn regrouping_is_durable_exact_and_distinct_from_settlement() {
    let asserter = Asserter::new();
    let (publication, lane, _, _, _temp) = fixture(&asserter, false).await;
    let old = command(&lane, 4, 5, 7);
    let old_work = Work::new(&old, &publication).unwrap();
    let old_dir = publication.journal.work_directory(&old_work);
    assert!(!old_dir.exists());
    anchor(&asserter, &lane);
    publication.regroup_unproved(&old, 2).await.unwrap();
    let bytes = read_private(&old_dir.join("work.json")).unwrap().unwrap();
    let marker = read_private(&old_dir.join("regrouped.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&marker).unwrap(),
        json!({
            "schema_version": 1, "work_hash":keccak256(&bytes), "native_proved":2,
            "native_batch_hash":B256::repeat_byte(2)
        })
    );
    assert!(!old_dir.join("superseded.json").exists() && !old_dir.join("confirmed.json").exists());

    let (config, rpc, policy, timelock) = (
        publication.config.clone(),
        publication.provider.clone(),
        publication.confirmation_policy,
        publication.timelock,
    );
    drop(publication);
    let publication = ServicePublication::new(config, rpc, policy, timelock).unwrap();
    anchor(&asserter, &lane);
    publication.regroup_unproved(&old, 2).await.unwrap();
    assert_eq!(
        read_private(&old_dir.join("regrouped.json"))
            .unwrap()
            .unwrap(),
        marker
    );
    assert_eq!(
        publication
            .regrouped_candidates()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![(4, 5)]
    );
    anchor(&asserter, &lane);
    assert!(publication.is_regrouped(&old).await.unwrap());
    assert!(
        !publication
            .is_regrouped(&command(&lane, 4, 5, 8))
            .await
            .unwrap()
    );
    assert!(
        publication
            .journal
            .work_for_batch(&old.as_ref()[0], &publication)
            .await
            .unwrap()
            .is_none()
    );
    let replacement = command(&lane, 3, 5, 9);
    let replacement_work = Work::new(&replacement, &publication).unwrap();
    publication.journal.stage(&replacement_work, 16).unwrap();
    assert_eq!(
        publication
            .journal
            .work_for_batch(&old.as_ref()[0], &publication)
            .await
            .unwrap(),
        Some(replacement_work)
    );
}

#[tokio::test]
async fn eligible_regeneration_reopens_same_proof_after_native_frontier_moves() {
    let asserter = Asserter::new();
    let (publication, lane, _, _, _temp) = fixture(&asserter, false).await;
    let old = command(&lane, 4, 5, 7);
    let work = Work::new(&old, &publication).unwrap();
    anchor(&asserter, &lane);
    publication.regroup_unproved(&old, 2).await.unwrap();
    let (_, directory, digest) = publication.journal.stage(&work, 64).unwrap();
    for frontier in [2u64, 3] {
        anchor_at(&asserter, &lane, frontier, frontier, B256::repeat_byte(2));
        publication
            .reactivate_if_next(&work, &directory, digest)
            .await
            .unwrap();
        if frontier == 2 {
            anchor(&asserter, &lane);
        }
        assert_eq!(publication.is_regrouped(&old).await.unwrap(), frontier == 2);
    }
    assert_eq!(
        publication
            .journal
            .work_for_batch(&old.as_ref()[0], &publication)
            .await
            .unwrap(),
        Some(work)
    );
}

#[tokio::test]
async fn regrouping_refuses_unconfirmed_frontier_and_preserves_service_receipt_handoff() {
    let asserter = Asserter::new();
    let (publication, lane, mut handoff, _, _temp) = fixture(&asserter, false).await;
    let old = command(&lane, 4, 5, 7);
    let work = Work::new(&old, &publication).unwrap();
    let (_, directory, digest) = publication.journal.stage(&work, 64).unwrap();
    snapshot(&asserter, &lane, 2, 1);
    assert!(publication.regroup_unproved(&old, 2).await.is_err());
    assert!(!directory.join("regrouped.json").exists());
    anchor(&asserter, &lane);
    publication.regroup_unproved(&old, 2).await.unwrap();
    handoff.work_hash = digest;
    handoff.transaction_hash = B256::repeat_byte(15);
    handoff.sidecar.accepted_package.batchFrom = 4;
    handoff.sidecar.accepted_package.batchTo = 5;
    atomic_write(
        &directory.join("relay.json"),
        &serde_json::to_vec(&handoff).unwrap(),
    )
    .unwrap();
    asserter.push_success(&Value::Null);
    assert!(
        publication
            .journal
            .work_for_batch(&old.as_ref()[0], &publication)
            .await
            .unwrap()
            .is_none()
    );
    let matching = receipt(&work, &handoff);
    asserter.push_success(&matching);
    assert_eq!(
        publication
            .journal
            .work_for_batch(&old.as_ref()[0], &publication)
            .await
            .unwrap(),
        Some(work.clone())
    );
    assert!(!directory.join("confirmed.json").exists());

    let confirmed = Confirmed {
        schema_version: 1,
        work_hash: digest,
        transaction_hash: handoff.transaction_hash,
        block_hash: B256::repeat_byte(42),
        block_number: 42,
        calldata_hash: keccak256(&work.proof_data),
        package_hash: hash_package(&handoff.sidecar.accepted_package),
    };
    let bytes = serde_json::to_vec(&confirmed).unwrap();
    atomic_write(&directory.join("confirmed.json"), &bytes).unwrap();
    assert!(!publication.is_regrouped(&old).await.unwrap());
    anchor(&asserter, &lane);
    assert!(publication.regroup_unproved(&old, 2).await.is_err());
    assert_eq!(
        read_private(&directory.join("confirmed.json"))
            .unwrap()
            .unwrap(),
        bytes
    );
}

async fn journaled_wrappers(
    journal: &crate::prover_api::snark_proof_journal::SnarkProofJournal,
    lane: &Work,
) -> Vec<crate::prover_api::snark_proof_journal::JournaledProof> {
    let mut wrappers = Vec::new();
    for (from, to, generation) in [(4, 5, 7), (6, 7, 8)] {
        let wrapper = command(lane, from, to, generation);
        let proof = wrapper.preflight_inputs().1.clone();
        wrappers.push(journal.persist(wrapper.into(), proof).await.unwrap());
    }
    wrappers
}

#[tokio::test]
async fn startup_finishes_parked_snark_unlink_after_restart_or_a_later_native_advance() {
    use crate::prover_api::{
        snark_proof_journal::SnarkProofJournal, snark_proving_pipeline_step::prepare_startup_replay,
    };

    for frontier in [2, 3] {
        let asserter = Asserter::new();
        let (publication, lane, _, _, temp) = fixture(&asserter, false).await;
        let old = command(&lane, 4, 5, 7);
        let (journal, _) = SnarkProofJournal::open(temp.path()).await.unwrap();
        journaled_wrappers(&journal, &lane).await;
        anchor(&asserter, &lane);
        publication.regroup_unproved(&old, 2).await.unwrap();
        drop(journal);
        let (journal, _) = SnarkProofJournal::open(temp.path()).await.unwrap();
        let validated = journaled_wrappers(&journal, &lane).await;
        anchor_at(&asserter, &lane, frontier, frontier, B256::repeat_byte(2));
        let replay = prepare_startup_replay(
            &journal,
            Some(&publication),
            validated,
            frontier,
            7,
            4,
            64,
            false,
        )
        .await
        .unwrap();
        assert_eq!(replay.covered_ranges, vec![(6, 7)]);
        assert_eq!(replay.commands.len(), 1);
        assert_eq!(
            replay
                .plan
                .ranges()
                .map(|range| range.as_tuple())
                .collect::<Vec<_>>(),
            vec![(frontier + 1, 5)]
        );
        assert_eq!(journal.record_count().await, 1);
    }
}

#[tokio::test]
async fn historical_regrouping_intent_requires_the_same_confirmed_native_anchor() {
    let asserter = Asserter::new();
    let (publication, lane, _, _, _temp) = fixture(&asserter, false).await;
    let old = command(&lane, 4, 5, 7);
    anchor(&asserter, &lane);
    publication.regroup_unproved(&old, 2).await.unwrap();
    snapshot(&asserter, &lane, 1, 1);
    assert!(publication.is_regrouped(&old).await.is_err());
    anchor_at(&asserter, &lane, 3, 3, B256::repeat_byte(8));
    assert!(publication.is_regrouped(&old).await.is_err());
    let work = Work::new(&old, &publication).unwrap();
    let (_, directory, digest) = publication.journal.stage(&work, 64).unwrap();
    anchor_at(&asserter, &lane, 3, 3, B256::repeat_byte(8));
    assert!(
        publication
            .reactivate_if_next(&work, &directory, digest)
            .await
            .is_err()
    );
    assert!(directory.join("regrouped.json").exists());
}
