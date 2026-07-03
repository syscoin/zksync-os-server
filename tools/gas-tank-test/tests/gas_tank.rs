//! SYSCOIN: end-to-end forward-run tests for the zkSYS gas-tank fee flow.
//!
//! Run via ./run.sh, which points the crate at a patched zksync-os checkout
//! and bakes the test tank address (0x3333...33) into the generated
//! `syscoin_edge_da.rs` before executing.
//!
//! Covered:
//! - fee precharged from the tank at 1:1, native balance untouched by fees
//! - unused gas refunded to the tank ledger
//! - operator tip credited to the coinbase's tank balance
//! - base fee burned: totalCredits shrinks by exactly gas_used * base_fee
//! - insufficient credit falls back to the pre-verified native path, where
//!   the base fee is burned natively and the coinbase receives only the tip
//! - solvency across the precharge window: totalCredits is NOT reduced by
//!   the precharge mid-execution (it keeps backing the pending refund/tip,
//!   so a mid-tx burnSurplus() can never overburn), and is reduced exactly
//!   once at settlement by the burned base fee

use rig::alloy::primitives::{address, keccak256, Address, TxKind};
use rig::alloy::rpc::types::TransactionRequest;
use rig::forward_system::run::convert_alloy::FromAlloy;
use rig::ruint::aliases::{B160, B256, U256};
use rig::{BlockContext, TestingFramework};
use zksync_os_tests_common::zksync_tx::ZKsyncTxEnvelope;

/// Must match SYSCOIN_GAS_TANK_ADDRESS baked by run.sh.
const TANK: Address = address!("3333333333333333333333333333333333333333");
const COINBASE: Address = address!("1000000000000000000000000000000000000000");
/// rig's default BlockContext eip1559_basefee.
const BASE_FEE: u64 = 1000;
const GAS_PRICE: u64 = 1500;
const GAS_LIMIT: u64 = 100_000;

/// Solidity mapping slot 0: keccak256(pad32(account) || pad32(0)).
fn credit_key(account: Address) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[12..32].copy_from_slice(account.as_slice());
    U256::from_be_bytes(keccak256(preimage).0)
}

const TOTAL_CREDITS_KEY: u64 = 1;

fn slot_u256(tester: &mut TestingFramework, key: U256) -> U256 {
    tester
        .get_storage_slot(&TANK, key)
        .map(|v| U256::from_be_bytes(v.as_u8_array()))
        .unwrap_or(U256::ZERO)
}

fn b256(value: U256) -> B256 {
    B256::from_be_bytes(value.to_be_bytes::<32>())
}

fn simple_transfer_tx(
    tester: &mut TestingFramework,
    to: Address,
    value_wei: u64,
) -> (Address, ZKsyncTxEnvelope) {
    let wallet = tester.random_signer();
    let sender = wallet.address();
    let tx = ZKsyncTxEnvelope::from_eth_tx_from_req(
        TransactionRequest {
            to: Some(TxKind::Call(to)),
            gas: Some(GAS_LIMIT.into()),
            gas_price: Some(GAS_PRICE as u128),
            value: Some(rig::alloy::primitives::U256::from(value_wei)),
            nonce: Some(0),
            ..Default::default()
        },
        wallet,
    );
    (sender, tx)
}

#[test]
fn gas_tank_pays_fee_refunds_and_tips_in_credit() {
    let mut tester = TestingFramework::new();
    let recipient = address!("00000000000000000000000000000000000000bb");
    let (sender, tx) = simple_transfer_tx(&mut tester, recipient, 12_345);

    let initial_native = U256::from(1_000_000_000_000_000_u64);
    let initial_credit = U256::from(300_000_000_000_u64);
    let value = U256::from(12_345_u64);

    tester = tester
        .with_balance(sender, initial_native)
        .with_storage_slot(TANK, credit_key(sender), b256(initial_credit))
        .with_storage_slot(TANK, U256::from(TOTAL_CREDITS_KEY), b256(initial_credit))
        .with_block_context(BlockContext {
            coinbase: B160::from_alloy(COINBASE),
            ..Default::default()
        })
        .without_revm_consistency_check();

    let output = tester.execute_block(vec![tx]);
    assert_eq!(output.tx_results.len(), 1);
    let tx_output = output.tx_results[0].as_ref().expect("tx must not error");
    assert!(tx_output.is_success(), "tx must succeed");
    let gas_used = U256::from(tx_output.gas_used);
    assert!(gas_used > U256::ZERO && gas_used <= U256::from(GAS_LIMIT));

    // Native balances: value moved, fees never touched native.
    assert_eq!(
        tester.get_balance(&sender),
        initial_native - value,
        "fee must not be charged from the native balance"
    );
    assert_eq!(tester.get_balance(&recipient), value);
    assert_eq!(
        tester.get_balance(&COINBASE),
        U256::ZERO,
        "operator must not receive native fees for a tank-paid tx"
    );

    // Tank ledger: sender paid gas_used * gas_price (prepayment minus refund).
    let fee_charged = gas_used * U256::from(GAS_PRICE);
    assert_eq!(
        slot_u256(&mut tester, credit_key(sender)),
        initial_credit - fee_charged,
        "tank credit must pay exactly gas_used * gas_price"
    );

    // Operator tip is tank credit; base fee is burned.
    let tip = gas_used * U256::from(GAS_PRICE - BASE_FEE);
    assert_eq!(
        slot_u256(&mut tester, credit_key(COINBASE)),
        tip,
        "operator tip must be credited to the tank ledger"
    );

    // totalCredits shrinks by exactly the burned base fee, which becomes
    // surplus zkSYS in the tank contract for burnSurplus().
    let burned = gas_used * U256::from(BASE_FEE);
    assert_eq!(
        slot_u256(&mut tester, U256::from(TOTAL_CREDITS_KEY)),
        initial_credit - burned,
        "totalCredits must shrink by the burned base fee"
    );
}

#[test]
fn insufficient_tank_credit_falls_back_to_native() {
    let mut tester = TestingFramework::new();
    let recipient = address!("00000000000000000000000000000000000000bb");
    let (sender, tx) = simple_transfer_tx(&mut tester, recipient, 0);

    let initial_native = U256::from(1_000_000_000_000_000_u64);
    // Far below the fee prepayment (gas_limit * gas_price = 1.5e8).
    let initial_credit = U256::from(1_000_u64);

    tester = tester
        .with_balance(sender, initial_native)
        .with_storage_slot(TANK, credit_key(sender), b256(initial_credit))
        .with_storage_slot(TANK, U256::from(TOTAL_CREDITS_KEY), b256(initial_credit))
        .with_block_context(BlockContext {
            coinbase: B160::from_alloy(COINBASE),
            ..Default::default()
        })
        .without_revm_consistency_check();

    let output = tester.execute_block(vec![tx]);
    let tx_output = output.tx_results[0].as_ref().expect("tx must not error");
    assert!(tx_output.is_success());
    let gas_used = U256::from(tx_output.gas_used);

    // Tank untouched.
    assert_eq!(slot_u256(&mut tester, credit_key(sender)), initial_credit);
    assert_eq!(
        slot_u256(&mut tester, U256::from(TOTAL_CREDITS_KEY)),
        initial_credit
    );
    assert_eq!(slot_u256(&mut tester, credit_key(COINBASE)), U256::ZERO);

    // Native path: fee charged natively; base fee burned; coinbase gets tip.
    let fee_charged = gas_used * U256::from(GAS_PRICE);
    assert_eq!(tester.get_balance(&sender), initial_native - fee_charged);
    let tip = gas_used * U256::from(GAS_PRICE - BASE_FEE);
    assert_eq!(
        tester.get_balance(&COINBASE),
        tip,
        "coinbase must receive only the tip; the base fee is burned"
    );
}

#[test]
fn reverted_execution_still_charges_the_tank() {
    let mut tester = TestingFramework::new();
    // PUSH1 0 PUSH1 0 REVERT
    let reverter = address!("00000000000000000000000000000000000000cc");
    let (sender, tx) = simple_transfer_tx(&mut tester, reverter, 0);

    let initial_native = U256::from(1_000_000_000_000_000_u64);
    let initial_credit = U256::from(300_000_000_000_u64);

    tester = tester
        .with_evm_contract(reverter, &[0x60, 0x00, 0x60, 0x00, 0xfd])
        .with_balance(sender, initial_native)
        .with_storage_slot(TANK, credit_key(sender), b256(initial_credit))
        .with_storage_slot(TANK, U256::from(TOTAL_CREDITS_KEY), b256(initial_credit))
        .with_block_context(BlockContext {
            coinbase: B160::from_alloy(COINBASE),
            ..Default::default()
        })
        .without_revm_consistency_check();

    let output = tester.execute_block(vec![tx]);
    let tx_output = output.tx_results[0].as_ref().expect("tx must not error");
    assert!(!tx_output.is_success(), "call must revert");
    let gas_used = U256::from(tx_output.gas_used);
    assert!(gas_used > U256::ZERO);

    // The fee for consumed gas is charged from the tank even on revert.
    let fee_charged = gas_used * U256::from(GAS_PRICE);
    assert_eq!(
        slot_u256(&mut tester, credit_key(sender)),
        initial_credit - fee_charged
    );
    assert_eq!(tester.get_balance(&sender), initial_native, "native untouched");

    let tip = gas_used * U256::from(GAS_PRICE - BASE_FEE);
    assert_eq!(slot_u256(&mut tester, credit_key(COINBASE)), tip);
    let burned = gas_used * U256::from(BASE_FEE);
    assert_eq!(
        slot_u256(&mut tester, U256::from(TOTAL_CREDITS_KEY)),
        initial_credit - burned
    );
}

/// Regression for the review release-blocker: mid-execution, `totalCredits`
/// must still include the in-flight precharge (the bootloader debits only the
/// sender's credit), so the pending refund/tip can never be exposed as
/// burnable surplus to a mid-tx `burnSurplus()`. Settlement then reduces
/// `totalCredits` exactly once, by the burned base fee.
///
/// The probe deploys bytecode at the tank address that snapshots slot 1
/// (`totalCredits`) into scratch slot 2 when called, giving us the value the
/// tank contract itself would see during the transaction's execution.
#[test]
fn total_credits_not_reduced_mid_execution() {
    let mut tester = TestingFramework::new();
    let (sender, tx) = simple_transfer_tx(&mut tester, TANK, 0);

    let initial_native = U256::from(1_000_000_000_000_000_u64);
    let initial_credit = U256::from(300_000_000_000_u64);

    tester = tester
        // PUSH1 1 SLOAD PUSH1 2 SSTORE STOP: copy totalCredits into slot 2.
        .with_evm_contract(TANK, &[0x60, 0x01, 0x54, 0x60, 0x02, 0x55, 0x00])
        .with_balance(sender, initial_native)
        .with_storage_slot(TANK, credit_key(sender), b256(initial_credit))
        .with_storage_slot(TANK, U256::from(TOTAL_CREDITS_KEY), b256(initial_credit))
        .with_block_context(BlockContext {
            coinbase: B160::from_alloy(COINBASE),
            ..Default::default()
        })
        .without_revm_consistency_check();

    let output = tester.execute_block(vec![tx]);
    let tx_output = output.tx_results[0].as_ref().expect("tx must not error");
    assert!(tx_output.is_success(), "probe call must succeed");
    let gas_used = U256::from(tx_output.gas_used);

    // The mid-execution snapshot must show totalCredits still at its full
    // pre-tx value: the precharge debits the sender credit only. (Under the
    // vulnerable accounting this snapshot would read
    // initial_credit - gas_limit * gas_price instead.)
    assert_eq!(
        slot_u256(&mut tester, U256::from(2u64)),
        initial_credit,
        "totalCredits must not be reduced by the precharge mid-execution"
    );

    // Final ledger: sender paid the actual fee, coinbase got the tip, and
    // totalCredits shrank by exactly the burned base fee at settlement.
    let fee_charged = gas_used * U256::from(GAS_PRICE);
    assert_eq!(
        slot_u256(&mut tester, credit_key(sender)),
        initial_credit - fee_charged
    );
    let tip = gas_used * U256::from(GAS_PRICE - BASE_FEE);
    assert_eq!(slot_u256(&mut tester, credit_key(COINBASE)), tip);
    let burned = gas_used * U256::from(BASE_FEE);
    assert_eq!(
        slot_u256(&mut tester, U256::from(TOTAL_CREDITS_KEY)),
        initial_credit - burned
    );
    // Ledger conservation: totalCredits == sum of credit entries, so every
    // credit is exactly backed and nothing burnable is left un-accounted.
    assert_eq!(
        slot_u256(&mut tester, U256::from(TOTAL_CREDITS_KEY)),
        slot_u256(&mut tester, credit_key(sender)) + slot_u256(&mut tester, credit_key(COINBASE)),
        "totalCredits must equal the sum of account credits after settlement"
    );
}

#[test]
fn zero_credit_account_behaves_like_upstream() {
    let mut tester = TestingFramework::new();
    let recipient = address!("00000000000000000000000000000000000000bb");
    let (sender, tx) = simple_transfer_tx(&mut tester, recipient, 777);

    let initial_native = U256::from(1_000_000_000_000_000_u64);
    tester = tester
        .with_balance(sender, initial_native)
        .with_block_context(BlockContext {
            coinbase: B160::from_alloy(COINBASE),
            ..Default::default()
        })
        .without_revm_consistency_check();

    let output = tester.execute_block(vec![tx]);
    let tx_output = output.tx_results[0].as_ref().expect("tx must not error");
    assert!(tx_output.is_success());
    let gas_used = U256::from(tx_output.gas_used);

    let fee_charged = gas_used * U256::from(GAS_PRICE);
    assert_eq!(
        tester.get_balance(&sender),
        initial_native - fee_charged - U256::from(777u64)
    );
    assert_eq!(tester.get_balance(&recipient), U256::from(777u64));
}
