use alloy::eips::eip2930::AccessList;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{AccessListItem, TransactionRequest};
use zksync_os_integration_tests::Tester;
use zksync_os_integration_tests::assert_traits::ReceiptAssert;
use zksync_os_integration_tests::contracts::{
    AddressWarmingTest, BlockHashTest, PrecompileDelegateCallTest, RawBytecodeTest,
    RawEvmEdgeCaseTest, SelfdestructComboTest, SelfdestructDelegateCallTest,
    SelfdestructGasTest, SelfdestructNewAccountGasTest, WarmAfterRevertTest,
};

/// Wait for the REVM consistency checker to process recent blocks.
///
/// The checker runs asynchronously in the pipeline. If it detects a divergence
/// with `revert_on_divergence: true`, it panics the server process. We sleep
/// to give it time to catch up, then verify the server is still alive by
/// querying the RPC. If the server crashed, `get_block_number()` will fail.
async fn wait_for_revm_checker(tester: &Tester) -> anyhow::Result<()> {
    const REVM_CHECKER_WAIT_SECS: u64 = 30;
    tracing::info!(
        "Waiting {REVM_CHECKER_WAIT_SECS}s for REVM consistency checker to process blocks"
    );
    tokio::time::sleep(std::time::Duration::from_secs(REVM_CHECKER_WAIT_SECS)).await;

    // If the REVM checker panicked the server, this RPC call will fail.
    tester
        .l2_provider
        .get_block_number()
        .await
        .map_err(|e| anyhow::anyhow!("Server appears to have crashed (REVM divergence?): {e}"))?;

    tracing::info!("REVM consistency checker passed — server still healthy");
    Ok(())
}

// ============================================================================
// SELFDESTRUCT gas measurement
//
// Post-Cancun, SELFDESTRUCT transfers balance but does NOT destroy the
// contract (EIP-6780). Both zksync-os and REVM charge the same gas here
// (5,000 base + 2,600 cold + 25,000 NEWACCOUNT for empty beneficiary).
//
// The test measures gas around the selfdestruct call and stores it.
// This serves as a regression test — both implementations should agree.
// ============================================================================

#[test_log::test(tokio::test)]
async fn selfdestruct_gas_measurement() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = SelfdestructGasTest::deploy(tester.l2_provider.clone()).await?;

    // Fresh address = empty account (nonce=0, no code, balance=0)
    let fresh_beneficiary = Address::random();

    contract
        .testSelfdestructToEmpty(fresh_beneficiary)
        .value(U256::from(1_000_000))
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Bug: P256 precompile address warming
//
// zksync-os treats ALL precompile addresses (including P256 at 0x100) as
// always-warm, giving them free cold access. However, the REVM consistency
// checker explicitly does NOT pre-warm the P256 address because it is not
// part of the standard EIP-2929 precompile set (addresses 1–9).
//
// The test measures BALANCE gas for various address categories:
//   - Standard precompiles (1-9): warm in both → 100 gas (agree)
//   - P256 (0x100):
//       zksync-os: ~100 gas (treated as warm precompile)
//       REVM:      ~2,600 gas (cold, not in 1-9 set)
//
// The stored p256AccessGas value will differ, causing the REVM consistency
// checker to detect a divergence.
// ============================================================================

#[test_log::test(tokio::test)]
async fn eip2929_p256_address_warming() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = AddressWarmingTest::deploy(tester.l2_provider.clone()).await?;

    // Use a deterministic cold address so storage writes are reproducible
    let cold_address = Address::left_padding_from(&[0xde, 0xad]);

    contract
        .measureAll(cold_address)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    // Read back all gas measurements for diagnostics
    let origin_gas = contract.originAccessGas().call().await?;
    let self_gas = contract.selfAccessGas().call().await?;
    let cold_gas = contract.coldAccessGas().call().await?;
    let coinbase_gas = contract.coinbaseAccessGas().call().await?;
    let precompile_gas = contract.precompileAccessGas().call().await?;
    let p256_gas = contract.p256AccessGas().call().await?;

    tracing::info!(
        origin = %origin_gas,
        self_access = %self_gas,
        cold = %cold_gas,
        coinbase = %coinbase_gas,
        precompile = %precompile_gas,
        p256 = %p256_gas,
        "Address warming gas measurements"
    );

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Bug: Warm/cold access status reverted on call frame revert (EIP-2929)
//
// Per EIP-2929, access list entries (warm status) must persist even when the
// call frame that added them reverts.
//
// REVM has two-level warm tracking:
//   1. WarmAddresses (NOT reverted) — for EIP-2930 access list entries
//   2. Journal-level (reverted) — for execution-level warming
//
// The test puts accessListTarget in the access list with a storage slot, then
// CALLs a helper that touches coldTarget via BALANCE and reverts. After the
// revert, it measures BALANCE gas for both addresses.
//
// For accessListTarget (in access list):
//   REVM: warm via WarmAddresses (survives revert) → ~100 gas
//   zksync-os: may not have two-layer warm tracking → may differ
//
// For coldTarget (NOT in access list, only warmed inside reverting call):
//   Both REVM and zksync-os revert execution-level warming → ~2,600 gas
// ============================================================================

#[test_log::test(tokio::test)]
async fn eip2929_warm_persists_after_revert() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = WarmAfterRevertTest::deploy(tester.l2_provider.clone()).await?;

    // Address IN the access list — should stay warm via WarmAddresses in REVM
    let access_list_target = Address::left_padding_from(&[0xaa, 0xbb]);
    // Address NOT in the access list — only warmed inside the reverting call
    let cold_target = Address::left_padding_from(&[0xca, 0xfe]);

    // Access list: includes accessListTarget with a storage slot
    let slot_0 = B256::ZERO;
    let access_list = AccessList(vec![AccessListItem {
        address: access_list_target,
        storage_keys: vec![slot_0],
    }]);

    contract
        .testWarmAfterRevert(access_list_target, cold_target)
        .access_list(access_list)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    // Read back diagnostic values
    let call_success = contract.callSuccess().call().await?;
    let gas_with_al = contract.balanceGasWithAccessList().call().await?;
    let gas_without_al = contract.balanceGasWithoutAccessList().call().await?;

    tracing::info!(
        call_success,
        gas_with_access_list = %gas_with_al,
        gas_without_access_list = %gas_without_al,
        "Warm-after-revert diagnostics"
    );

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Precompile delegate-call consistency
//
// Delegate-calls every precompile (ecrecover, SHA-256, RIPEMD-160, identity,
// modexp, BN254 add/mul/pairing, P256Verify) with various inputs. For each
// call, stores keccak256(success ++ gasUsed ++ returnData) in a storage slot.
//
// Any difference in precompile behavior (gas cost, return data, success/failure)
// between zksync-os and REVM will surface as a storage mismatch.
// ============================================================================

#[test_log::test(tokio::test)]
async fn precompile_delegatecall_consistency() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = PrecompileDelegateCallTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .gas(30_000_000)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    tracing::info!(total_tests = %total, "Precompile delegate-call tests completed");

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// SELFDESTRUCT + DELEGATECALL edge cases
//
// Exercises 14 scenarios covering post-Cancun SELFDESTRUCT semantics combined
// with DELEGATECALL. For each scenario, observable state (balances, code
// existence, return values) is hashed and stored. Any divergence between
// zksync-os and REVM will surface as a storage mismatch.
//
// Scenarios include:
//   - SELFDESTRUCT to self, to existing, to non-existent accounts
//   - Double SELFDESTRUCT in a single call
//   - SELFDESTRUCT then continue execution (post-Cancun)
//   - SELFDESTRUCT inside a reverting CALL frame
//   - CREATE + SELFDESTRUCT in same tx (EIP-6780 actual destruction)
//   - CREATE + SELFDESTRUCT inside a reverting frame
//   - DELEGATECALL to a selfdestruct implementation
//   - DELEGATECALL selfdestruct to self
//   - Nested DELEGATECALL chain ending in selfdestruct
//   - Calling a selfdestructed contract again in the same tx
// ============================================================================

#[test_log::test(tokio::test)]
async fn selfdestruct_delegatecall_edge_cases() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = SelfdestructDelegateCallTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .value(U256::from(5_000_000_000_000_000_000u64)) // 5 ETH
        .gas(60_000_000)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    tracing::info!(total_tests = %total, "SELFDESTRUCT + DELEGATECALL edge case tests completed");

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// SELFDESTRUCT + DELEGATECALL + constructor combo tests
//
// Exercises 10 novel scenarios combining constructor execution with
// DELEGATECALL and SELFDESTRUCT:
//   - Constructor delegatecalls to SELFDESTRUCT impl (EIP-6780 via DC)
//   - Constructor delegatecalls SD then continues storing values
//   - Nested CREATE chain (grandchild selfdestructs via EIP-6780)
//   - CREATE2 + EIP-6780 destroy + re-CREATE2 (address reuse)
//   - DELEGATECALL to code that CREATEs an ephemeral child
//   - SELFDESTRUCT to a precompile address (edge beneficiary)
//   - Double DELEGATECALL selfdestruct from same contract
//   - Recursive selfdestruct then read storage
//   - Pre-fund address via selfdestruct, then CREATE2 at it
//   - Impl contract survives after delegatecall SD
// ============================================================================

#[test_log::test(tokio::test)]
async fn selfdestruct_constructor_combo_tests() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = SelfdestructComboTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .value(U256::from(5_000_000_000_000_000_000u64)) // 5 ETH
        .gas(80_000_000)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    tracing::info!(total_tests = %total, "SELFDESTRUCT + constructor combo tests completed");

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Raw bytecode tests (impossible to generate via Solidity)
//
// Deploys raw EVM bytecode and exercises patterns that Solidity's codegen
// never produces:
//   - Minimal 2-byte contracts (CALLER SD, PUSH0 SD)
//   - Dead code after SELFDESTRUCT opcode
//   - SELFBALANCE as selfdestruct beneficiary (0x47FF)
//   - Init code that selfdestructs (no RETURN, no runtime)
//   - Init code that SSTOREs then selfdestructs
//   - SELFDESTRUCT inside STATICCALL
//   - EXTCODE ops on selfdestructed contracts within same tx
//   - Double SELFDESTRUCT in raw bytecode (33FF33FF)
//   - CREATE2 with SD init code + re-CREATE2 at same address
// ============================================================================

#[test_log::test(tokio::test)]
async fn raw_bytecode_selfdestruct_tests() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = RawBytecodeTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .value(U256::from(3_000_000_000_000_000_000u64)) // 3 ETH
        .gas(80_000_000)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    tracing::info!(total_tests = %total, "Raw bytecode tests completed");

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Raw EVM edge case tests (non-SELFDESTRUCT bytecode patterns)
//
// Deploys raw EVM bytecode that Solidity's codegen can never produce:
//   - CALLCODE opcode (0xF2) vs DELEGATECALL comparison
//   - PC opcode (0x58) at multiple offsets
//   - INVALID opcode (0xFE) behavior
//   - JUMPDEST (0x5B) inside PUSH data (invalid jump target)
//   - Runtime-computed JUMP from calldata
//   - CODECOPY / EXTCODECOPY self-inspection
//   - BYTE opcode edge cases
//   - RETURNDATACOPY out-of-bounds
//   - MSTORE8 single-byte writes
// ============================================================================

#[test_log::test(tokio::test)]
async fn raw_evm_edge_case_tests() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = RawEvmEdgeCaseTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .value(U256::from(1_000_000_000_000_000_000u64)) // 1 ETH
        .gas(80_000_000)
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    tracing::info!(total_tests = %total, "Raw EVM edge case tests completed");

    wait_for_revm_checker(&tester).await?;
    Ok(())
}

// ============================================================================
// Bug found_1.md: SELFDESTRUCT charges 25,000 NEWACCOUNT post-Cancun
//
// Per EIP-6780, go-ethereum skips the 25,000 NEWACCOUNT charge for
// SELFDESTRUCT post-Cancun (returns early before the charge).
//
// This test measures gas for SELFDESTRUCT to an empty beneficiary vs a
// non-empty beneficiary. The gas difference (~25,000) is stored in storage.
//
// The REVM consistency checker compares storage diffs between zksync-os
// and REVM. If the two disagree on the NEWACCOUNT charge, the stored
// gas measurements would differ and the checker would flag a mismatch.
// ============================================================================

#[test_log::test(tokio::test)]
async fn selfdestruct_newaccount_gas_post_cancun() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    let contract = SelfdestructNewAccountGasTest::deploy(tester.l2_provider.clone()).await?;

    // Empty beneficiary: fresh address, no balance/nonce/code
    let empty_beneficiary = Address::random();
    // Non-empty beneficiary: use the test contract itself (has code + nonce)
    let non_empty_beneficiary = *contract.address();

    contract
        .measure(empty_beneficiary, non_empty_beneficiary)
        .value(U256::from(200_000_000_000_000_000u64)) // 0.2 ETH
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let gas_to_empty = contract.gasToEmpty().call().await?;
    let gas_to_non_empty = contract.gasToNonEmpty().call().await?;
    let gas_difference = contract.gasDifference().call().await?;

    tracing::info!(
        gas_to_empty = %gas_to_empty,
        gas_to_non_empty = %gas_to_non_empty,
        gas_difference = %gas_difference,
        "SELFDESTRUCT NEWACCOUNT gas measurement"
    );

    Ok(())
}

// ============================================================================
// BLOCKHASH opcode consistency
//
// Tests that the BLOCKHASH opcode returns correct values matching REVM:
//   - blockhash(block.number) → 0 (current block hash not available)
//   - blockhash(block.number + N) → 0 (future blocks)
//   - blockhash(block.number - 1) → non-zero previous block hash
//   - blockhash(0) → genesis hash (if within 256-block window)
//   - blockhash(block.number - 257) → 0 (beyond 256-block window)
//   - Multiple consecutive block hashes (all distinct)
//   - Calling blockhash twice for the same block → identical results
//   - Boundary test: last valid (N-256) vs first invalid (N-257)
//
// The contract stores keccak256 of each test's observable outputs in
// storage. Any divergence between zksync-os and REVM will surface as
// a storage mismatch in the consistency checker.
// ============================================================================

#[test_log::test(tokio::test)]
async fn blockhash_opcode_consistency() -> anyhow::Result<()> {
    let tester = Tester::setup().await?;

    // Produce 260+ blocks so all BLOCKHASH edge cases are exercised:
    // - block 0 falls outside the 256-block window → returns 0
    // - boundary test: block.number-256 (last valid) vs block.number-257 (first invalid)
    let target_block = 260u64;
    let current_block = tester.l2_provider.get_block_number().await?;
    let blocks_needed = target_block.saturating_sub(current_block);

    if blocks_needed > 0 {
        tracing::info!(
            current_block,
            blocks_needed,
            "Padding blocks to reach block {target_block}"
        );
        let recipient = Address::random();
        for i in 0..blocks_needed {
            tester
                .l2_provider
                .send_transaction(
                    TransactionRequest::default()
                        .with_to(recipient)
                        .with_value(U256::from(1)),
                )
                .await?
                .expect_successful_receipt()
                .await?;

            if (i + 1) % 50 == 0 {
                tracing::info!(blocks_produced = i + 1, "Padding progress");
            }
        }
        let final_block = tester.l2_provider.get_block_number().await?;
        tracing::info!(final_block, "Block padding complete");
    }

    let contract = BlockHashTest::deploy(tester.l2_provider.clone()).await?;

    contract
        .runAll()
        .send()
        .await?
        .expect_successful_receipt()
        .await?;

    let total = contract.totalTests().call().await?;
    let executed_at = contract.executedAtBlock().call().await?;
    let hash_current = contract.hashAtCurrent().call().await?;
    let hash_previous = contract.hashAtPrevious().call().await?;
    let hash_boundary = contract.hashAtBoundary().call().await?;

    tracing::info!(
        total_tests = %total,
        executed_at_block = %executed_at,
        "blockhash(block.number)     = {:?}", hash_current
    );
    tracing::info!(
        "blockhash(block.number - 1)   = {:?}", hash_previous
    );
    tracing::info!(
        "blockhash(block.number - 256) = {:?}", hash_boundary
    );

    wait_for_revm_checker(&tester).await?;
    Ok(())
}
