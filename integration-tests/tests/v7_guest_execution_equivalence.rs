#![cfg(feature = "prover-tests")]

//! Native-V6 / published-V7 execution regression for the Syscoin SLH-DSA precompile.
//!
//! This is deliberately a simulator and public-input commitment test, not a proof test. The V7
//! guest exposes only the batch public input in registers x10..x17; it does not expose per-call
//! status, return data, or gas. We therefore assert those observations against the native
//! `RunBlockForwardV6` execution, persist them in contract state, and then require the exact
//! published V7 multiblock guest to return the public input committing to that resulting state.
//! A direct field-by-field guest comparison would require changing the guest ABI, app and VK.
//! This slow test is ignored and must be invoked manually; it is not a normal CI gate.

use alloy::network::{ReceiptResponse, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::{SolEvent, SolValue};
use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Client, StatusCode};
use risc_v_simulator_prev::{
    abstractions::non_determinism::QuasiUARTSource,
    runner::run_simple_with_entry_point_and_non_determimism_source,
    sim::{BinarySource, SimulatorConfig},
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;
use zksync_os_integration_tests::{
    SettlementLayer, TestCase, Tester, assert_traits::ReceiptAssert,
};
use zksync_os_server::default_protocol_version::PROTOCOL_VERSION_V31_0;
use zksync_os_storage_api::PersistedBatch;
use zksync_os_types::ProvingVersion;

alloy::sol! {
    event ObservationRecorded(
        uint256 indexed index,
        uint256 gasLimit,
        bool success,
        uint256 returnDataLength,
        bytes32 returnWord,
        uint256 gasUsed
    );
}

const PRECOMPILE_GAS: u64 = 45_000;
const PROBE_GAS_LIMITS: [u64; 3] = [PRECOMPILE_GAS - 1, PRECOMPILE_GAS, PRECOMPILE_GAS + 1];
const PIPELINE_TIMEOUT: Duration = Duration::from_secs(300);
const V7_MULTIBLOCK_SHA256: &str =
    "1487dd6070b75f43f433499f3ab2910e23dfacc24319bb09c1ed43375483e7b5";
const PROBE_INIT_SHA256: &str = "c32125d5daea32d36b2b447406ce73af4d9e2df006a5b23ed953b89b18e76294";

/*
Reproduce the probe bytecode by saving this source as `src/SlhBoundaryProbe.sol` and running:

    forge build --root . --use 0.8.28 --evm-version prague \
      --optimize --optimizer-runs 200 --no-metadata
    jq -r '.bytecode.object, .deployedBytecode.object' \
      out/SlhBoundaryProbe.sol/SlhBoundaryProbe.json

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;
contract SlhBoundaryProbe {
    struct Observation {
        bool success;
        uint256 returnDataLength;
        bytes32 returnWord;
        uint256 gasUsed;
    }
    Observation[3] public observations;
    event ObservationRecorded(
        uint256 indexed index, uint256 gasLimit, bool success,
        uint256 returnDataLength, bytes32 returnWord, uint256 gasUsed
    );
    constructor(bytes memory input) {
        uint256[3] memory limits = [uint256(44_999), 45_000, 45_001];
        address target = address(0x101);
        // Exclude the EIP-2929 cold-account charge from the three measurements.
        assembly { pop(staticcall(0, target, 0, 0, 0, 0)) }
        for (uint256 i = 0; i < limits.length; ++i) {
            uint256 beforeGas = gasleft();
            (bool success, bytes memory output) = target.staticcall{gas: limits[i]}(input);
            uint256 gasUsed = beforeGas - gasleft();
            bytes32 returnWord;
            if (output.length >= 32) {
                assembly { returnWord := mload(add(output, 32)) }
            }
            observations[i] = Observation(success, output.length, returnWord, gasUsed);
            emit ObservationRecorded(i, limits[i], success, output.length, returnWord, gasUsed);
        }
    }
}
```

The SHA-256 assertion below pins the 959-byte creation code. Runtime exposes
`observations(uint256)` for audit/debug only.
*/
const PROBE_INIT_CODE_HEX: &str = "608060405234801561000f575f5ffd5b506040516103bf3803806103bf83398101604081905261002e91610210565b6040805160608101825261afc7815261afc8602082015261afc9918101919091526101015f8080808481fa505f5b60038110156101f3575f5a90505f5f846001600160a01b0316868560038110610087576100876102c0565b60200201518860405161009a91906102d4565b5f604051808303818686fa925050503d805f81146100d3576040519150601f19603f3d011682016040523d82523d5f602084013e6100d8565b606091505b50915091505f5a6100e990856102ea565b90505f60208351106100fc575060208201515b6040518060800160405280851515815260200184518152602001828152602001838152505f8760038110610132576101326102c0565b825160049190910291909101805460ff1916911515919091178155602082015160018201556040820151600282015560609091015160039182015586907f30ef50265e9fed20254d030852dbebddd7e2ba444c4f5d3e71c96651b38eeb53908a90839081106101a3576101a36102c0565b602002015185516040516101db9291899187908990948552921515602085015260408401919091526060830152608082015260a00190565b60405180910390a2505050505080600101905061005c565b5050505061030f565b634e487b7160e01b5f52604160045260245ffd5b5f60208284031215610220575f5ffd5b81516001600160401b03811115610235575f5ffd5b8201601f81018413610245575f5ffd5b80516001600160401b0381111561025e5761025e6101fc565b604051601f8201601f19908116603f011681016001600160401b038111828210171561028c5761028c6101fc565b6040528181528282016020018610156102a3575f5ffd5b8160208401602083015e5f91810160200191909152949350505050565b634e487b7160e01b5f52603260045260245ffd5b5f82518060208501845e5f920191825250919050565b8181038181111561030957634e487b7160e01b5f52601160045260245ffd5b92915050565b60a48061031b5f395ff3fe6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063252c09d714602a575b5f5ffd5b60396035366004608e565b605f565b604080519415158552602085019390935291830152606082015260800160405180910390f35b5f8160038110606c575f80fd5b6004020180546001820154600283015460039093015460ff9092169350919084565b5f60208284031215609d575f5ffd5b503591905056";
const PROBE_RUNTIME_CODE_HEX: &str = "6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063252c09d714602a575b5f5ffd5b60396035366004608e565b605f565b604080519415158552602085019390935291830152606082015260800160405180910390f35b5f8160038110606c575f80fd5b6004020180546001820154600283015460039093015460ff9092169350919084565b5f60208284031215609d575f5ffd5b503591905056";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlhVector {
    id: String,
    status: String,
    pk_seed: String,
    pk_root: String,
    message: String,
    signature: String,
    precompile_input_sha256: String,
    expected: bool,
}

#[derive(Debug, Deserialize)]
struct FriJob {
    batch_number: u64,
    vk_hash: String,
}

#[derive(Debug, Deserialize)]
struct FriJobState {
    fri_job: FriJob,
}

#[derive(Debug, Deserialize)]
struct BatchDataPayload {
    batch_number: u64,
    vk_hash: String,
    prover_input: String,
}

#[derive(Debug)]
struct NativeObservation {
    success: bool,
    return_data_length: U256,
    return_word: U256,
    gas_used: U256,
}

fn decode_hex(label: &str, value: &str) -> anyhow::Result<Vec<u8>> {
    alloy::hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .with_context(|| format!("invalid hex for {label}"))
}

fn canonical_slh_input() -> anyhow::Result<Vec<u8>> {
    let vector: SlhVector = serde_json::from_str(include_str!(
        "../../contracts/test/vectors/slh_dsa_sha2_128_24_sp800_230_ipd_counter0.json"
    ))?;
    ensure!(
        vector.id == "slh-dsa-sha2-128-24-sp800-230-ipd-counter0-v1"
            && vector.status == "canonical-reproducible-conformance"
            && vector.expected,
        "unexpected SLH-DSA conformance fixture metadata"
    );

    let mut input = Vec::with_capacity(3_952);
    input.extend(decode_hex("pkSeed", &vector.pk_seed)?);
    input.extend(decode_hex("pkRoot", &vector.pk_root)?);
    input.extend(decode_hex("message", &vector.message)?);
    input.extend(decode_hex("signature", &vector.signature)?);
    ensure!(input.len() == 3_952, "unexpected precompile input length");
    ensure!(
        format!("0x{}", alloy::hex::encode(Sha256::digest(&input)))
            == vector.precompile_input_sha256,
        "canonical precompile input hash changed"
    );
    Ok(input)
}

fn decode_witness(payload: BatchDataPayload) -> anyhow::Result<(u64, Vec<u32>)> {
    ensure!(
        payload.vk_hash == ProvingVersion::V7.vk_hash(),
        "batch {} has unexpected VK {}",
        payload.batch_number,
        payload.vk_hash
    );
    let bytes = BASE64
        .decode(payload.prover_input)
        .context("invalid base64 FRI witness")?;
    ensure!(!bytes.is_empty(), "batch witness is empty (PIG disabled?)");
    ensure!(bytes.len() % 4 == 0, "FRI witness is not LE-u32 aligned");
    let words = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    Ok((payload.batch_number, words))
}

async fn capture_pending_witnesses(
    http: &Client,
    prover_api: &str,
    captured: &mut BTreeMap<u64, Vec<u32>>,
) -> anyhow::Result<()> {
    let states = http
        .get(format!("{prover_api}/prover-jobs/v1/status/fri"))
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<FriJobState>>()
        .await?;
    for state in states {
        ensure!(
            state.fri_job.vk_hash == ProvingVersion::V7.vk_hash(),
            "batch {} has unexpected VK {}",
            state.fri_job.batch_number,
            state.fri_job.vk_hash
        );
        if captured.contains_key(&state.fri_job.batch_number) {
            continue;
        }
        let response = http
            .get(format!(
                "{prover_api}/prover-jobs/v1/FRI/{}/peek",
                state.fri_job.batch_number
            ))
            .send()
            .await?;
        if response.status() == StatusCode::NO_CONTENT {
            continue;
        }
        let (batch_number, words) = decode_witness(response.error_for_status()?.json().await?)?;
        captured.insert(batch_number, words);
    }
    Ok(())
}

async fn advance_fake_snark(http: &Client, prover_api: &str) -> anyhow::Result<()> {
    // With fake FRI enabled and fake SNARK disabled, this endpoint consumes pending fake FRI
    // proofs as a side effect and emits the fake SNARK command needed to execute the batch.
    let response = http
        .post(format!("{prover_api}/prover-jobs/v1/SNARK/pick"))
        .query(&[
            ("id", "v7-guest-equivalence"),
            ("supported_vk_hashes", ProvingVersion::V7.vk_hash()),
        ])
        .send()
        .await?;
    ensure!(
        response.status() == StatusCode::NO_CONTENT,
        "unexpected real SNARK job while advancing fake pipeline: {}",
        response.status()
    );
    Ok(())
}

async fn wait_for_persisted_batch(
    tester: &Tester,
    http: &Client,
    prover_api: &str,
    block_number: u64,
    captured: &mut BTreeMap<u64, Vec<u32>>,
) -> anyhow::Result<PersistedBatch> {
    let deadline = Instant::now() + PIPELINE_TIMEOUT;
    loop {
        capture_pending_witnesses(http, prover_api, captured).await?;
        advance_fake_snark(http, prover_api).await?;

        let result = tester
            .l2_zk_provider
            .client()
            .request::<_, PersistedBatch>("unstable_getBatchByBlockNumber", (block_number,))
            .await;
        match result {
            Ok(batch) => return Ok(batch),
            Err(err)
                if err.as_error_resp().is_some_and(|response| {
                    response.code == -32603 && response.message.contains("has not been finalized")
                }) => {}
            Err(err) => return Err(err.into()),
        }
        ensure!(
            Instant::now() < deadline,
            "block {block_number} was not persisted within {PIPELINE_TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_observations(
    tester: &Tester,
    contract: Address,
) -> anyhow::Result<[NativeObservation; 3]> {
    let mut values = Vec::with_capacity(3);
    for index in 0..3u64 {
        let slot = index * 4;
        let success = tester
            .l2_provider
            .get_storage_at(contract, U256::from(slot))
            .await?;
        ensure!(success <= U256::ONE, "success slot is not Boolean");
        values.push(NativeObservation {
            success: success == U256::ONE,
            return_data_length: tester
                .l2_provider
                .get_storage_at(contract, U256::from(slot + 1))
                .await?,
            return_word: tester
                .l2_provider
                .get_storage_at(contract, U256::from(slot + 2))
                .await?,
            gas_used: tester
                .l2_provider
                .get_storage_at(contract, U256::from(slot + 3))
                .await?,
        });
    }
    values
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrong observation count"))
}

fn expected_public_input(previous: &PersistedBatch, current: &PersistedBatch) -> [u32; 8] {
    let hash = keccak256(
        [
            previous.batch_info.state_commitment.0,
            current.batch_info.state_commitment.0,
            current.batch_info.commitment.0,
        ]
        .concat(),
    );
    hash.0
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

#[test_log::test(tokio::test)]
#[ignore = "slow: launches settlement pipeline and simulates the exact published V7 guest"]
async fn native_v6_slh_boundary_matches_published_v7_guest_commitment() -> anyhow::Result<()> {
    ensure!(
        std::env::var("NEXTEST_PROFILE").as_deref() != Ok("no-pig"),
        "this test requires real prover-input generation; do not use NEXTEST_PROFILE=no-pig"
    );

    let app = zksync_os_multivm::apps::v7::MULTIBLOCK_BATCH;
    ensure!(
        alloy::hex::encode(Sha256::digest(app)) == V7_MULTIBLOCK_SHA256,
        "published V7 multiblock app changed"
    );
    let probe_init = decode_hex("probe init code", PROBE_INIT_CODE_HEX)?;
    ensure!(
        alloy::hex::encode(Sha256::digest(&probe_init)) == PROBE_INIT_SHA256,
        "boundary probe init code changed"
    );

    let env = TestCase {
        protocol_version: PROTOCOL_VERSION_V31_0,
        settlement_layer: SettlementLayer::L1,
    }
    .environment()
    .await?;
    let mut config = env.default_config().await?;
    config.prover_api_config.enabled = true;
    config.prover_api_config.fake_fri_provers.enabled = true;
    config.prover_api_config.fake_fri_provers.workers = 1;
    config.prover_api_config.fake_fri_provers.min_age = Duration::from_secs(5);
    config.prover_api_config.fake_fri_provers.compute_time = Duration::from_millis(100);
    config.prover_api_config.fake_fri_provers.timeout_frequency = 0.0;
    config.prover_api_config.fake_snark_provers.enabled = false;
    config.prover_input_generator_config.enable_input_generation = true;
    config.batcher_config.batch_timeout = Duration::from_secs(1);
    let tester = env.launch(config).await?;
    let prover_api = tester
        .prover_api_url()
        .context("prover API must be enabled")?;
    let http = Client::new();
    let mut captured = BTreeMap::new();

    // Finalize one batch first. PersistedBatch storage does not expose genesis as a predecessor,
    // while the V7 public input needs the immediately previous state commitment.
    let warm_receipt = tester
        .l2_provider
        .send_transaction(
            TransactionRequest::default()
                .with_to(Address::random())
                .with_value(U256::ONE),
        )
        .await?
        .expect_successful_receipt()
        .await?;
    ensure!(warm_receipt.status(), "warm-up transaction reverted");
    let warm_block = warm_receipt
        .block_number()
        .context("warm-up receipt is missing block number")?;
    let warm_batch =
        wait_for_persisted_batch(&tester, &http, &prover_api, warm_block, &mut captured).await?;

    let input = canonical_slh_input()?;
    let sender = tester.l2_wallet.default_signer().address();
    let nonce_before = tester.l2_provider.get_transaction_count(sender).await?;
    let balance_before = tester.l2_provider.get_balance(sender).await?;
    let mut deployment = probe_init;
    deployment.extend((Bytes::from(input),).abi_encode_params());
    let request = TransactionRequest::default()
        .with_deploy_code(Bytes::from(deployment))
        .with_gas_limit(2_000_000);
    let estimated_gas = tester.l2_provider.estimate_gas(request.clone()).await?;
    let receipt = tester
        .l2_provider
        .send_transaction(request)
        .await?
        .expect_successful_receipt()
        .await?;
    ensure!(receipt.status(), "boundary probe deployment reverted");
    ensure!(
        receipt.gas_used <= estimated_gas,
        "gas estimate was too small"
    );
    ensure!(
        receipt.from == sender && receipt.to.is_none(),
        "unexpected deployment sender/to"
    );
    let contract = receipt
        .contract_address()
        .context("deployment receipt has no contract address")?;
    let probe_block = receipt
        .block_number()
        .context("deployment receipt is missing block number")?;

    let nonce_after = tester.l2_provider.get_transaction_count(sender).await?;
    let balance_after = tester.l2_provider.get_balance(sender).await?;
    ensure!(
        nonce_after == nonce_before + 1,
        "sender nonce delta changed"
    );
    let expected_fee = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    ensure!(
        balance_before - balance_after == expected_fee,
        "sender balance delta does not equal receipt fee"
    );
    ensure!(
        tester.l2_provider.get_balance(contract).await? == U256::ZERO,
        "probe contract unexpectedly holds value"
    );
    ensure!(
        tester.l2_provider.get_code_at(contract).await?
            == decode_hex("probe runtime", PROBE_RUNTIME_CODE_HEX)?,
        "deployed probe runtime changed"
    );

    let observations = read_observations(&tester, contract).await?;
    ensure!(
        !observations[0].success
            && observations[0].return_data_length == U256::ZERO
            && observations[0].return_word == U256::ZERO,
        "cost-1 call must exhaust its forwarded gas and return no bytes"
    );
    for (index, observation) in observations[1..].iter().enumerate() {
        ensure!(
            observation.success
                && observation.return_data_length == U256::from(32)
                && observation.return_word == U256::ONE,
            "cost+{index} valid call returned an unexpected result"
        );
    }
    // Both successful calls pay the same precompile charge. Solidity's dynamic return buffer can
    // add a few memory-expansion gas between loop iterations, so compare the caller deltas within
    // a small bound rather than pretending they must be bytecode-instruction identical.
    let successful_gas_gap = if observations[1].gas_used >= observations[2].gas_used {
        observations[1].gas_used - observations[2].gas_used
    } else {
        observations[2].gas_used - observations[1].gas_used
    };
    ensure!(
        successful_gas_gap <= U256::from(32),
        "one extra unit of forwarded gas materially changed successful-call consumption"
    );
    ensure!(
        observations[0].gas_used < observations[1].gas_used,
        "cost-1 OOG call did not consume less caller gas than the successful call"
    );

    ensure!(
        receipt.logs().len() == 3,
        "probe must emit exactly three observations"
    );
    let return_one = B256::from(U256::ONE.to_be_bytes::<32>());
    for (index, log) in receipt.logs().iter().enumerate() {
        ensure!(
            log.address() == contract,
            "observation emitted by wrong contract"
        );
        let event = ObservationRecorded::decode_log(&log.inner)?;
        let observation = &observations[index];
        ensure!(
            event.index == U256::from(index)
                && event.gasLimit == U256::from(PROBE_GAS_LIMITS[index])
                && event.success == observation.success
                && event.returnDataLength == observation.return_data_length
                && event.returnWord == if index == 0 { B256::ZERO } else { return_one }
                && event.gasUsed == observation.gas_used,
            "event/storage observation mismatch at boundary {index}"
        );
    }

    let block = tester
        .l2_provider
        .get_block_by_number(probe_block.into())
        .await?
        .context("probe block is missing")?;
    ensure!(
        block
            .transactions
            .hashes()
            .any(|hash| hash == receipt.transaction_hash),
        "probe transaction is missing from its receipt block"
    );
    ensure!(
        block.header.gas_used >= receipt.gas_used,
        "block gas is below probe receipt gas"
    );

    let current =
        wait_for_persisted_batch(&tester, &http, &prover_api, probe_block, &mut captured).await?;
    ensure!(
        current.batch_info.batch_number > warm_batch.batch_info.batch_number,
        "probe was not sealed into a post-warm-up batch"
    );
    ensure!(
        current.block_range.contains(&probe_block),
        "persisted batch does not contain probe block"
    );
    ensure!(
        current.execute_sl_block_number.is_some(),
        "probe batch was not executed on the settlement layer"
    );
    let prior_block = current
        .first_block_number()
        .checked_sub(1)
        .context("probe batch has no predecessor block")?;
    let previous = tester
        .l2_zk_provider
        .client()
        .request::<_, PersistedBatch>("unstable_getBatchByBlockNumber", (prior_block,))
        .await?;
    let expected_pi = expected_public_input(&previous, &current);
    let witness = captured
        .remove(&current.batch_info.batch_number)
        .with_context(|| {
            format!(
                "missed real PIG witness for probe batch {} before fake FRI pickup",
                current.batch_info.batch_number
            )
        })?;

    // Stop node work before the CPU-heavy guest simulation. This exact app/witness pair is what a
    // V7 prover would execute; comparing its output to the persisted-batch PI binds all asserted
    // native effects above without claiming the guest exposes those fields individually.
    let _stopped = tester.stop().await?;
    let guest_pi = tokio::task::spawn_blocking(move || {
        let expected_final_pc = execution_utils_prev::find_binary_exit_point(app);
        let config = SimulatorConfig::new(BinarySource::Slice(app), 0, 1usize << 36, None);
        let result = run_simple_with_entry_point_and_non_determimism_source(
            config,
            QuasiUARTSource::new_with_reads(witness),
        );
        assert!(
            result.reached_end,
            "published V7 guest exhausted the simulator cycle bound"
        );
        assert_eq!(
            result.state.pc, expected_final_pc,
            "published V7 guest stopped outside its canonical exit"
        );
        #[allow(deprecated)]
        let output: [u32; 8] = result.state.registers[10..18].try_into().unwrap();
        output
    })
    .await
    .context("V7 guest simulator panicked")?;
    ensure!(
        guest_pi == expected_pi,
        "published V7 guest PI does not match the persisted native-V6 batch commitment"
    );
    Ok(())
}
