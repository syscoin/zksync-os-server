//! SYSCOIN: Reproduce the guest's dynamic zkSYS gas-tank fee branch in the
//! diagnostic REVM replay.
//!
//! REVM has no concept of the bootloader fee ledger. A tank-paid transaction
//! must therefore start REVM from the guest's post-precharge credit value and,
//! after the payload, replace REVM's native fee movement with the guest's
//! refund / tip / burn ledger settlement. This is state-model adaptation only;
//! transaction admission remains unchanged.
//!
//! The native pre-injection is deliberately paired with REVM's ordinary
//! validation path. It is deducted before the first payload opcode, so it is
//! never payload-visible, while retaining canonical caller code / nonce checks
//! and EIP-2929 warmth. Supporting transactions without the guest's current
//! native max-fee collateral would instead require an explicit sponsored-fee
//! handler mode; the service-transaction shortcut skips those invariants.

use alloy::consensus::Transaction;
use alloy::eips::eip4844::DATA_GAS_PER_BLOB;
use alloy::primitives::{Address, U256, keccak256};
use anyhow::{Context, ensure};
use revm::database::CacheDB;
use revm::{Database, DatabaseRef};
use zk_ee::utils::u256_try_to_u64;
use zk_os_forward_system::run::syscoin_gas_tank_checker::{
    CALLDATA_NON_ZERO_BYTE_TOKEN_FACTOR, CALLDATA_ZERO_BYTE_TOKEN_FACTOR, L2TxIntrinsicNativeInput,
    ResourcesForTx, SYSCOIN_GAS_TANK_CONDITIONAL_COMPUTATIONAL_NATIVE_COST,
    SYSCOIN_GAS_TANK_INTRINSIC_PUBDATA, SYSCOIN_GAS_TANK_PROBE_COMPUTATIONAL_NATIVE_COST,
    calculate_l2_tx_intrinsic_computational_native_resources, calculate_l2_tx_intrinsic_pubdata,
    calculate_tx_intrinsic_gas, create_resources_for_tx,
};
use zk_os_forward_system::system::system_types::ForwardRunningSystem;
use zksync_os_storage_api::BlockContext;
use zksync_os_types::{L2Envelope, SYSCOIN_GAS_TANK_ADDRESS, ZkEnvelope, ZkTransaction};

const TOTAL_CREDITS_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

struct TankResourceBudget {
    resources: ResourcesForTx<ForwardRunningSystem>,
    native_per_pubdata: u64,
    intrinsic_computational_native: u64,
    intrinsic_pubdata: u64,
}

impl TankResourceBudget {
    fn try_reserve(&mut self, additional_native: u64, additional_pubdata: u64) -> bool {
        let Some(next_native) = self
            .intrinsic_computational_native
            .checked_add(additional_native)
        else {
            return false;
        };
        let Some(next_pubdata) = self.intrinsic_pubdata.checked_add(additional_pubdata) else {
            return false;
        };
        if !self.resources.try_reserve_conditional_intrinsic(
            self.native_per_pubdata,
            additional_native,
            additional_pubdata,
        ) {
            return false;
        }
        self.intrinsic_computational_native = next_native;
        self.intrinsic_pubdata = next_pubdata;
        true
    }

    fn try_reserve_probe(&mut self) -> bool {
        self.try_reserve(SYSCOIN_GAS_TANK_PROBE_COMPUTATIONAL_NATIVE_COST, 0)
    }

    fn try_reserve_success(&mut self) -> bool {
        self.try_reserve(
            SYSCOIN_GAS_TANK_CONDITIONAL_COMPUTATIONAL_NATIVE_COST,
            SYSCOIN_GAS_TANK_INTRINSIC_PUBDATA,
        )
    }
}

/// Deferred settlement for a transaction whose guest fee precharge selected
/// the gas tank.
pub(crate) struct GasTankRevmPlan {
    sender: Address,
    coinbase: Address,
    sender_credit_slot: U256,
    coinbase_credit_slot: U256,
    gas_limit: u64,
    gas_used: u64,
    gas_price: U256,
    base_fee: U256,
    fee_to_prepay: U256,
    revm_gas_price: U256,
}

impl GasTankRevmPlan {
    /// Derive and apply the exact guest precharge branch before REVM opens its
    /// transaction journal. Inserting the precharged value into `CacheDB`
    /// makes it REVM's EIP-2200 original value while preserving normal access
    /// list warmth and payload rollback behavior.
    pub(crate) fn prepare<DB>(
        db: &mut CacheDB<DB>,
        tx: &ZkTransaction,
        block_context: &BlockContext,
        gas_used: u64,
        revm_block_basefee: u64,
        revm_blob_gasprice: u128,
    ) -> anyhow::Result<Option<Self>>
    where
        DB: DatabaseRef,
        DB::Error: std::error::Error + Send + Sync + 'static,
    {
        let ZkEnvelope::L2(l2_tx) = tx.envelope() else {
            return Ok(None);
        };
        ensure!(
            gas_used <= tx.gas_limit(),
            "gas-tank REVM replay gas_used exceeds gas_limit for transaction {}",
            tx.hash()
        );

        // SYSCOIN: The guest validates native max-fee collateral before its
        // fee source is selected. Check the exact same bound before any
        // synthetic REVM balance is added; otherwise the checker could make an
        // undercollateralized tank transaction pass REVM validation.
        let required_native_balance =
            guest_required_native_balance_for_l2(l2_tx, block_context.blob_fee)?;
        let sender = tx.signer();
        let sender_native_balance = db
            .load_account(sender)
            .map_err(anyhow::Error::new)?
            .info
            .balance;
        ensure!(
            sender_native_balance >= required_native_balance,
            "gas-tank transaction {} lacks native max-fee collateral: required {required_native_balance}, balance {sender_native_balance}",
            tx.hash()
        );

        let gas_price = guest_gas_price(l2_tx, block_context.eip1559_basefee)?;
        let native_per_gas = native_per_gas(gas_price, block_context.native_price)?;
        if native_per_gas == 0 || gas_price.is_zero() {
            return Ok(None);
        }

        let blob_gas_used = l2_tx.blob_gas_used().unwrap_or_default();
        let gas_fee = gas_price
            .checked_mul(U256::from(tx.gas_limit()))
            .context("gas-tank gas prepayment overflow")?;
        let guest_blob_fee = block_context
            .blob_fee
            .checked_mul(U256::from(blob_gas_used))
            .context("gas-tank blob prepayment overflow")?;
        let fee_to_prepay = gas_fee
            .checked_add(guest_blob_fee)
            .context("gas-tank total prepayment overflow")?;
        if fee_to_prepay.is_zero() {
            return Ok(None);
        }

        let native_per_pubdata =
            native_per_pubdata(block_context.pubdata_price, block_context.native_price)?;
        let mut resource_budget =
            resource_budget(l2_tx, tx.gas_limit(), native_per_gas, native_per_pubdata)?;
        if !resource_budget.try_reserve_probe() {
            return Ok(None);
        }

        let sender_credit_slot = credit_slot(sender);
        let credit = read_tank_slot(db, sender_credit_slot)?;
        let Some(new_credit) = credit.checked_sub(fee_to_prepay) else {
            return Ok(None);
        };

        // SYSCOIN: The guest reserves the larger branch only after the credit
        // probe succeeds. Failure deliberately retains native-fee fallback.
        if !resource_budget.try_reserve_success() {
            return Ok(None);
        }

        let total_credits = read_tank_slot(db, TOTAL_CREDITS_SLOT)?;
        ensure!(
            total_credits >= credit,
            "gas tank totalCredits below sender credit during REVM replay"
        );

        // SYSCOIN: REVM deducts the full gas-limit fee before the payload,
        // whereas the guest's tank branch never touches the native balance.
        // Inject that exact REVM fee first so BALANCE / SELFBALANCE and value
        // checks observe the same payload prestate as the guest.
        let revm_effective_gas_price = if revm_block_basefee == 0 {
            0
        } else {
            l2_tx.effective_gas_price(Some(revm_block_basefee))
        };
        let revm_gas_price = U256::from(revm_effective_gas_price);
        ensure!(
            revm_gas_price == gas_price,
            "guest and REVM effective gas prices differ during gas-tank replay"
        );
        let revm_gas_prepayment = revm_gas_price
            .checked_mul(U256::from(tx.gas_limit()))
            .context("gas-tank REVM gas prepayment overflow")?;
        let revm_blob_fee = revm_blob_gasprice
            .checked_mul(u128::from(blob_gas_used))
            .map(U256::from)
            .context("gas-tank REVM blob prepayment overflow")?;
        let revm_upfront_fee = revm_gas_prepayment
            .checked_add(revm_blob_fee)
            .context("gas-tank REVM upfront fee overflow")?;
        let injected_sender_balance = sender_native_balance
            .checked_add(revm_upfront_fee)
            .context("native balance overflow while preparing gas-tank replay")?;

        write_tank_slot(db, sender_credit_slot, new_credit)?;
        db.load_account(sender)
            .map_err(anyhow::Error::new)?
            .info
            .balance = injected_sender_balance;
        Ok(Some(Self {
            sender,
            coinbase: block_context.coinbase,
            sender_credit_slot,
            coinbase_credit_slot: credit_slot(block_context.coinbase),
            gas_limit: tx.gas_limit(),
            gas_used,
            gas_price,
            base_fee: block_context.eip1559_basefee,
            fee_to_prepay,
            revm_gas_price,
        }))
    }

    /// Replace REVM's native fee movement with the guest's fee-ledger
    /// settlement. Every ledger operation reads the current post-payload value,
    /// so a transaction that calls the tank itself composes exactly as it does
    /// in the guest; reverted payload writes have already been rolled back by
    /// REVM before this method runs.
    pub(crate) fn settle<DB>(self, db: &mut CacheDB<DB>) -> anyhow::Result<()>
    where
        DB: DatabaseRef,
        DB::Error: std::error::Error + Send + Sync + 'static,
    {
        let revm_unused_refund = self
            .revm_gas_price
            .checked_mul(U256::from(self.gas_limit.saturating_sub(self.gas_used)))
            .context("gas-tank REVM unused-gas refund overflow")?;
        let revm_beneficiary_reward = self
            .revm_gas_price
            .checked_mul(U256::from(self.gas_used))
            .context("gas-tank REVM beneficiary reward overflow")?;

        // The pre-payload injection was consumed by REVM's upfront charge.
        // Remove only REVM's later unused-gas refund and beneficiary reward;
        // blob fees have no post-execution credit.
        sub_balance(db, self.sender, revm_unused_refund)?;
        sub_balance(db, self.coinbase, revm_beneficiary_reward)?;

        let refund = self
            .gas_price
            .checked_mul(U256::from(self.gas_limit.saturating_sub(self.gas_used)))
            .context("gas-tank refund overflow")?;
        let tip_price = self.gas_price.saturating_sub(self.base_fee);
        let tip = tip_price
            .checked_mul(U256::from(self.gas_used))
            .context("gas-tank operator tip overflow")?;
        let burned = self
            .fee_to_prepay
            .checked_sub(refund)
            .and_then(|remaining| remaining.checked_sub(tip))
            .context("gas-tank burned fee underflow")?;

        add_tank_slot(db, self.sender_credit_slot, refund)?;
        add_tank_slot(db, self.coinbase_credit_slot, tip)?;
        sub_tank_slot(db, TOTAL_CREDITS_SLOT, burned)?;
        Ok(())
    }
}

// SYSCOIN: Mirror `Transaction::required_balance()` from the pinned V32
// guest. This is deliberately based on fee caps, not the effective prices
// later injected for REVM execution.
fn guest_required_native_balance_for_l2(
    tx: &L2Envelope,
    block_blob_base_fee: U256,
) -> anyhow::Result<U256> {
    let blob_fee_cap = if tx.is_eip4844() {
        Some((
            tx.blob_count()
                .context("EIP-4844 transaction has no blob count")?,
            tx.max_fee_per_blob_gas()
                .context("EIP-4844 transaction has no max fee per blob gas")?,
        ))
    } else {
        None
    };
    guest_required_native_balance(
        tx.value(),
        tx.gas_limit(),
        tx.max_fee_per_gas(),
        blob_fee_cap,
        block_blob_base_fee,
    )
}

fn guest_required_native_balance(
    value: U256,
    gas_limit: u64,
    max_fee_per_gas: u128,
    blob_fee_cap: Option<(u64, u128)>,
    block_blob_base_fee: U256,
) -> anyhow::Result<U256> {
    let gas_fee = U256::from(max_fee_per_gas)
        .checked_mul(U256::from(gas_limit))
        .context("native max gas fee overflow")?;
    let mut required = value
        .checked_add(gas_fee)
        .context("native value plus max gas fee overflow")?;

    if let Some((blob_count, max_fee_per_blob_gas)) = blob_fee_cap {
        let max_fee_per_blob_gas = U256::from(max_fee_per_blob_gas);
        ensure!(
            block_blob_base_fee <= max_fee_per_blob_gas,
            "blob base fee exceeds transaction max fee per blob gas"
        );
        let blob_gas = blob_count
            .checked_mul(DATA_GAS_PER_BLOB)
            .context("blob gas count overflow")?;
        let blob_fee = max_fee_per_blob_gas
            .checked_mul(U256::from(blob_gas))
            .context("native max blob fee overflow")?;
        required = required
            .checked_add(blob_fee)
            .context("native total required balance overflow")?;
    }

    Ok(required)
}

fn guest_gas_price<T: Transaction>(tx: &T, base_fee: U256) -> anyhow::Result<U256> {
    if base_fee.is_zero() {
        return Ok(U256::ZERO);
    }
    let max_fee = U256::from(tx.max_fee_per_gas());
    let max_priority = U256::from(
        tx.max_priority_fee_per_gas()
            .unwrap_or_else(|| tx.max_fee_per_gas()),
    );
    ensure!(max_priority <= max_fee, "priority fee exceeds max fee");
    ensure!(base_fee <= max_fee, "base fee exceeds max fee");
    let priority = max_priority.min(max_fee.saturating_sub(base_fee));
    Ok(base_fee.saturating_add(priority).min(max_fee))
}

fn native_per_gas(gas_price: U256, native_price: U256) -> anyhow::Result<u64> {
    if native_price.is_zero() {
        return Ok(0);
    }
    u256_try_to_u64(&gas_price.div_ceil(native_price))
        .context("native resources are too expensive for gas-tank replay")
}

fn native_per_pubdata(pubdata_price: U256, native_price: U256) -> anyhow::Result<u64> {
    let ratio = pubdata_price.checked_div(native_price).unwrap_or_default();
    u256_try_to_u64(&ratio).context("pubdata is too expensive for gas-tank replay")
}

fn resource_budget<T: Transaction>(
    tx: &T,
    gas_limit: u64,
    native_per_gas: u64,
    native_per_pubdata: u64,
) -> anyhow::Result<TankResourceBudget> {
    let calldata_len = u64::try_from(tx.input().len()).context("calldata length exceeds u64")?;
    let zero_bytes = tx.input().iter().filter(|byte| **byte == 0).count() as u64;
    let nonzero_bytes = calldata_len.saturating_sub(zero_bytes);
    let calldata_tokens = zero_bytes
        .saturating_mul(CALLDATA_ZERO_BYTE_TOKEN_FACTOR)
        .saturating_add(nonzero_bytes.saturating_mul(CALLDATA_NON_ZERO_BYTE_TOKEN_FACTOR));
    let access_list_accounts = tx.access_list().map_or(0, |list| list.len() as u64);
    let access_list_storages = tx
        .access_list()
        .map_or(0, |list| list.storage_keys_count() as u64);
    let authorization_list_num = tx.authorization_count().unwrap_or_default();
    let blob_versioned_hashes_num = tx.blob_count().unwrap_or_default();

    // Standard server L2 envelopes cannot carry the guest-only FRI statement
    // list, so its count is exactly zero here.
    let statement_versioned_hashes_num = 0;
    let intrinsic_gas = calculate_tx_intrinsic_gas(
        calldata_len,
        calldata_tokens,
        tx.is_create(),
        access_list_accounts,
        access_list_storages,
        authorization_list_num,
        statement_versioned_hashes_num,
    );
    let intrinsic_computational_native =
        calculate_l2_tx_intrinsic_computational_native_resources(&L2TxIntrinsicNativeInput {
            calldata_byte_length: calldata_len,
            access_list_accounts,
            access_list_storages,
            authorization_list_num,
            blob_versioned_hashes_num,
            statement_versioned_hashes_num,
            is_service: false,
            free_native: false,
        });
    let intrinsic_pubdata = calculate_l2_tx_intrinsic_pubdata(authorization_list_num, false);
    let (resources, charge_error) = create_resources_for_tx::<ForwardRunningSystem>(
        gas_limit,
        false,
        native_per_gas.saturating_mul(gas_limit),
        native_per_pubdata,
        intrinsic_gas,
        intrinsic_computational_native,
        intrinsic_pubdata,
    );
    ensure!(
        charge_error.is_none(),
        "sealed transaction failed reconstructed guest intrinsic accounting: {charge_error:?}"
    );
    Ok(TankResourceBudget {
        resources,
        native_per_pubdata,
        intrinsic_computational_native,
        intrinsic_pubdata,
    })
}

fn credit_slot(account: Address) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[12..32].copy_from_slice(account.as_slice());
    U256::from_be_bytes(keccak256(preimage).0)
}

fn read_tank_slot<DB>(db: &mut CacheDB<DB>, slot: U256) -> anyhow::Result<U256>
where
    DB: DatabaseRef,
    DB::Error: std::error::Error + Send + Sync + 'static,
{
    Database::storage(db, SYSCOIN_GAS_TANK_ADDRESS, slot).map_err(anyhow::Error::new)
}

fn write_tank_slot<DB>(db: &mut CacheDB<DB>, slot: U256, value: U256) -> anyhow::Result<()>
where
    DB: DatabaseRef,
    DB::Error: std::error::Error + Send + Sync + 'static,
{
    db.insert_account_storage(SYSCOIN_GAS_TANK_ADDRESS, slot, value)
        .map_err(anyhow::Error::new)
}

fn add_tank_slot<DB>(db: &mut CacheDB<DB>, slot: U256, amount: U256) -> anyhow::Result<()>
where
    DB: DatabaseRef,
    DB::Error: std::error::Error + Send + Sync + 'static,
{
    if amount.is_zero() {
        return Ok(());
    }
    let current = read_tank_slot(db, slot)?;
    let next = current
        .checked_add(amount)
        .context("gas-tank credit overflow")?;
    write_tank_slot(db, slot, next)
}

fn sub_tank_slot<DB>(db: &mut CacheDB<DB>, slot: U256, amount: U256) -> anyhow::Result<()>
where
    DB: DatabaseRef,
    DB::Error: std::error::Error + Send + Sync + 'static,
{
    if amount.is_zero() {
        return Ok(());
    }
    let current = read_tank_slot(db, slot)?;
    let next = current
        .checked_sub(amount)
        .context("gas-tank totalCredits underflow")?;
    write_tank_slot(db, slot, next)
}

fn sub_balance<DB>(db: &mut CacheDB<DB>, account: Address, amount: U256) -> anyhow::Result<()>
where
    DB: DatabaseRef,
    DB::Error: std::error::Error + Send + Sync + 'static,
{
    if amount.is_zero() {
        return Ok(());
    }
    let account = db.load_account(account).map_err(anyhow::Error::new)?;
    account.info.balance = account
        .info
        .balance
        .checked_sub(amount)
        .context("native balance underflow while adapting gas-tank replay")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::{Signed, TxEip1559, TxEip4844, TxLegacy};
    use alloy::eips::eip2930::AccessList;
    use alloy::primitives::{B256, Bytes, Signature, TxKind};
    use revm::ExecuteCommitEvm;
    use revm::context_interface::ContextTr;
    use revm::database::EmptyDB;
    use revm::state::{AccountInfo, Bytecode};
    use zksync_os_revm::{DefaultZk, ZkBuilder, ZkContext, ZkSpecId};

    fn legacy_tx(gas_limit: u64, gas_price: u128) -> Signed<TxLegacy> {
        legacy_tx_to(
            gas_limit,
            gas_price,
            TxKind::Call(Address::with_last_byte(0x11)),
        )
    }

    fn legacy_tx_to(gas_limit: u64, gas_price: u128, to: TxKind) -> Signed<TxLegacy> {
        Signed::new_unchecked(
            TxLegacy {
                chain_id: Some(57_057),
                nonce: 0,
                gas_price,
                gas_limit,
                to,
                value: U256::ZERO,
                input: Bytes::new(),
            },
            Signature::new(U256::ONE, U256::ONE, false),
            B256::ZERO,
        )
    }

    fn set_balance(db: &mut CacheDB<EmptyDB>, account: Address, balance: U256) {
        db.insert_account_info(
            account,
            AccountInfo {
                balance,
                ..Default::default()
            },
        );
    }

    fn test_plan(
        sender: Address,
        coinbase: Address,
        gas_limit: u64,
        gas_used: u64,
        gas_price: u64,
        base_fee: u64,
    ) -> GasTankRevmPlan {
        GasTankRevmPlan {
            sender,
            coinbase,
            sender_credit_slot: credit_slot(sender),
            coinbase_credit_slot: credit_slot(coinbase),
            gas_limit,
            gas_used,
            gas_price: U256::from(gas_price),
            base_fee: U256::from(base_fee),
            fee_to_prepay: U256::from(gas_limit) * U256::from(gas_price),
            revm_gas_price: U256::from(gas_price),
        }
    }

    #[test]
    fn probe_can_fit_when_success_branch_does_not() {
        let gas_limit = 100_000;
        let tx = legacy_tx(gas_limit, 1);
        let base_native = calculate_l2_tx_intrinsic_computational_native_resources(
            &L2TxIntrinsicNativeInput::default(),
        );
        let target_native = base_native
            + SYSCOIN_GAS_TANK_PROBE_COMPUTATIONAL_NATIVE_COST
            + SYSCOIN_GAS_TANK_CONDITIONAL_COMPUTATIONAL_NATIVE_COST / 2;
        let native_per_gas = target_native.div_ceil(gas_limit);
        let mut budget = resource_budget(&tx, gas_limit, native_per_gas, 0).unwrap();

        assert!(budget.try_reserve_probe());
        assert!(!budget.try_reserve_success());
    }

    #[test]
    fn required_native_balance_uses_fee_caps_value_and_blob_gas() {
        let value = U256::from(7);
        assert_eq!(
            guest_required_native_balance(value, 3, 5, None, U256::from(99)).unwrap(),
            U256::from(22)
        );

        let blob_count = 2;
        let max_fee_per_blob_gas = 11;
        let expected = value
            + U256::from(3 * 5)
            + U256::from(blob_count * DATA_GAS_PER_BLOB) * U256::from(max_fee_per_blob_gas);
        assert_eq!(
            guest_required_native_balance(
                value,
                3,
                5,
                Some((blob_count, max_fee_per_blob_gas)),
                U256::from(10),
            )
            .unwrap(),
            expected
        );
    }

    #[test]
    fn typed_l2_envelopes_select_the_guest_collateral_formula() {
        let signature = Signature::new(U256::ONE, U256::ONE, false);
        let dynamic_fee = L2Envelope::from(Signed::new_unchecked(
            TxEip1559 {
                chain_id: 57_057,
                nonce: 0,
                gas_limit: 100,
                max_fee_per_gas: 1_000,
                max_priority_fee_per_gas: 7,
                to: TxKind::Call(Address::with_last_byte(0x11)),
                value: U256::from(9),
                access_list: AccessList::default(),
                input: Bytes::new(),
            },
            signature,
            B256::ZERO,
        ));
        assert_eq!(
            guest_required_native_balance_for_l2(&dynamic_fee, U256::from(999)).unwrap(),
            U256::from(100_009)
        );

        let mut versioned_hash = [0u8; 32];
        versioned_hash[0] = 1;
        let blob = L2Envelope::from(Signed::new_unchecked(
            TxEip4844 {
                chain_id: 57_057,
                nonce: 0,
                gas_limit: 100,
                max_fee_per_gas: 1_000,
                max_priority_fee_per_gas: 7,
                to: Address::with_last_byte(0x11),
                value: U256::from(9),
                access_list: AccessList::default(),
                blob_versioned_hashes: vec![B256::from(versioned_hash)],
                max_fee_per_blob_gas: 11,
                input: Bytes::new(),
            },
            signature,
            B256::ZERO,
        ));
        let expected = U256::from(100_009) + U256::from(DATA_GAS_PER_BLOB * 11);
        assert_eq!(
            guest_required_native_balance_for_l2(&blob, U256::from(10)).unwrap(),
            expected
        );
        assert!(guest_required_native_balance_for_l2(&blob, U256::from(12)).is_err());
    }

    #[test]
    fn required_native_balance_rejects_blob_cap_and_checked_overflow() {
        assert!(
            guest_required_native_balance(U256::ZERO, 1, 1, Some((1, 9)), U256::from(10),).is_err()
        );
        assert!(guest_required_native_balance(U256::MAX, 1, 1, None, U256::ONE).is_err());
        assert!(
            guest_required_native_balance(U256::ZERO, 1, 1, Some((u64::MAX, 1)), U256::ONE,)
                .is_err()
        );
    }

    #[test]
    fn prepare_rejects_native_undercollateralization_before_mutation() {
        let sender = Address::with_last_byte(0x12);
        let coinbase = Address::with_last_byte(0x36);
        let gas_limit = 100_000;
        let gas_price = 1_000u128;
        let required_native = U256::from(gas_limit) * U256::from(gas_price);
        let starting_credit = U256::from(1_000_000_000_000_000_000u64);
        let transaction = ZkTransaction::from(Recovered::new_unchecked(
            L2Envelope::from(legacy_tx(gas_limit, gas_price)),
            sender,
        ));
        let block_context = BlockContext {
            eip1559_basefee: U256::from(gas_price),
            native_price: U256::ONE,
            pubdata_price: U256::ZERO,
            blob_fee: U256::ONE,
            coinbase,
            ..Default::default()
        };

        for starting_native in [U256::ZERO, required_native - U256::ONE] {
            let mut db = CacheDB::new(EmptyDB::default());
            set_balance(&mut db, sender, starting_native);
            write_tank_slot(&mut db, credit_slot(sender), starting_credit).unwrap();
            write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, starting_credit).unwrap();

            let error = GasTankRevmPlan::prepare(
                &mut db,
                &transaction,
                &block_context,
                21_000,
                gas_price as u64,
                1,
            )
            .err()
            .expect("undercollateralized transaction must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("lacks native max-fee collateral")
            );
            assert_eq!(
                db.load_account(sender).unwrap().info.balance,
                starting_native
            );
            assert_eq!(
                read_tank_slot(&mut db, credit_slot(sender)).unwrap(),
                starting_credit
            );
            assert_eq!(
                read_tank_slot(&mut db, TOTAL_CREDITS_SLOT).unwrap(),
                starting_credit
            );
        }
    }

    #[test]
    fn prepare_rejects_synthetic_balance_overflow_before_mutation() {
        let sender = Address::with_last_byte(0x12);
        let coinbase = Address::with_last_byte(0x36);
        let gas_limit = 100_000;
        let gas_price = 1_000u128;
        let starting_credit = U256::from(1_000_000_000_000_000_000u64);
        let transaction = ZkTransaction::from(Recovered::new_unchecked(
            L2Envelope::from(legacy_tx(gas_limit, gas_price)),
            sender,
        ));
        let block_context = BlockContext {
            eip1559_basefee: U256::from(gas_price),
            native_price: U256::ONE,
            pubdata_price: U256::ZERO,
            blob_fee: U256::ONE,
            coinbase,
            ..Default::default()
        };
        let mut db = CacheDB::new(EmptyDB::default());
        set_balance(&mut db, sender, U256::MAX);
        write_tank_slot(&mut db, credit_slot(sender), starting_credit).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, starting_credit).unwrap();

        let error = GasTankRevmPlan::prepare(
            &mut db,
            &transaction,
            &block_context,
            21_000,
            gas_price as u64,
            1,
        )
        .err()
        .expect("synthetic balance overflow must fail closed");
        assert!(error.to_string().contains("native balance overflow"));
        assert_eq!(db.load_account(sender).unwrap().info.balance, U256::MAX);
        assert_eq!(
            read_tank_slot(&mut db, credit_slot(sender)).unwrap(),
            starting_credit
        );
        assert_eq!(
            read_tank_slot(&mut db, TOTAL_CREDITS_SLOT).unwrap(),
            starting_credit
        );
    }

    #[test]
    fn prepare_injects_revm_upfront_fee_before_payload() {
        let sender = Address::with_last_byte(0x12);
        let coinbase = Address::with_last_byte(0x36);
        let gas_limit = 100_000;
        let gas_price = 1_000u128;
        // SYSCOIN: Exact equality with the guest's max-fee collateral bound is
        // accepted; only the later synthetic REVM prepayment is additional.
        let starting_native = U256::from(gas_limit) * U256::from(gas_price);
        let starting_credit = U256::from(1_000_000_000_000_000_000u64);
        let transaction = ZkTransaction::from(Recovered::new_unchecked(
            L2Envelope::from(legacy_tx(gas_limit, gas_price)),
            sender,
        ));
        let block_context = BlockContext {
            eip1559_basefee: U256::from(gas_price),
            native_price: U256::ONE,
            pubdata_price: U256::ZERO,
            blob_fee: U256::ONE,
            coinbase,
            ..Default::default()
        };
        let mut db = CacheDB::new(EmptyDB::default());
        set_balance(&mut db, sender, starting_native);
        write_tank_slot(&mut db, credit_slot(sender), starting_credit).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, starting_credit).unwrap();

        let plan = GasTankRevmPlan::prepare(
            &mut db,
            &transaction,
            &block_context,
            21_000,
            gas_price as u64,
            1,
        )
        .unwrap();

        assert!(plan.is_some());
        let upfront = U256::from(gas_limit) * U256::from(gas_price);
        assert_eq!(
            db.load_account(sender).unwrap().info.balance,
            starting_native + upfront
        );
        assert_eq!(
            read_tank_slot(&mut db, credit_slot(sender)).unwrap(),
            starting_credit - upfront
        );
    }

    #[test]
    fn payload_balance_observes_native_prestate_not_injected_fee() {
        use crate::helpers::zk_tx_into_revm_tx;

        let sender = Address::with_last_byte(0x12);
        let coinbase = Address::with_last_byte(0x36);
        let target = Address::with_last_byte(0x44);
        let gas_limit = 100_000;
        let gas_used = 50_000;
        let gas_price = 1_000u128;
        let starting_native = U256::from(500_000_000u64);
        let starting_credit = U256::from(1_000_000_000_000_000_000u64);
        let transaction = ZkTransaction::from(Recovered::new_unchecked(
            L2Envelope::from(legacy_tx_to(gas_limit, gas_price, TxKind::Call(target))),
            sender,
        ));
        let block_context = BlockContext {
            eip1559_basefee: U256::from(gas_price),
            native_price: U256::ONE,
            pubdata_price: U256::ZERO,
            blob_fee: U256::ONE,
            coinbase,
            gas_limit,
            chain_id: 57_057,
            ..Default::default()
        };

        let mut db = CacheDB::new(EmptyDB::default());
        set_balance(&mut db, sender, starting_native);
        // CALLER; BALANCE; PUSH0; SSTORE; STOP. Slot zero records the balance
        // visible to payload bytecode before any post-execution refund.
        db.insert_account_info(
            target,
            AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from_static(&[
                0x33, 0x31, 0x5f, 0x55, 0x00,
            ]))),
        );
        write_tank_slot(&mut db, credit_slot(sender), starting_credit).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, starting_credit).unwrap();

        let mut evm = ZkContext::<EmptyDB>::default()
            .with_db(db)
            .modify_cfg_chained(|cfg| {
                cfg.chain_id = block_context.chain_id;
                cfg.spec = ZkSpecId::AtlasV3;
            })
            .modify_block_chained(|block| {
                block.basefee = gas_price as u64;
                block.beneficiary = coinbase;
                block.gas_limit = gas_limit;
            })
            .build_zk();
        let plan = GasTankRevmPlan::prepare(
            evm.0.db_mut(),
            &transaction,
            &block_context,
            gas_used,
            gas_price as u64,
            1,
        )
        .unwrap()
        .expect("funded credit must select the tank branch");
        let revm_tx = zk_tx_into_revm_tx(&transaction, gas_used, true, gas_limit, None).unwrap();

        evm.transact_commit(revm_tx).unwrap();
        plan.settle(evm.0.db_mut()).unwrap();

        assert_eq!(
            Database::storage(evm.0.db_mut(), target, U256::ZERO).unwrap(),
            starting_native
        );
        assert_eq!(
            evm.0.db_mut().load_account(sender).unwrap().info.balance,
            starting_native
        );
        assert_eq!(
            evm.0.db_mut().load_account(coinbase).unwrap().info.balance,
            U256::ZERO
        );
        let spent = U256::from(gas_used) * U256::from(gas_price);
        assert_eq!(
            read_tank_slot(evm.0.db_mut(), credit_slot(sender)).unwrap(),
            starting_credit - spent
        );
    }

    #[test]
    fn block_81_vector_replaces_native_fee_with_tank_burn() {
        let sender = Address::with_last_byte(0xd5);
        let coinbase = Address::with_last_byte(0x36);
        let gas_limit = 21_165;
        let gas_used = 21_000;
        let gas_price = 136_350_750;
        let pre_credit = U256::from(1_000_000_000_000_000_000_000u128);
        let prepaid = U256::from(gas_limit) * U256::from(gas_price);
        let spent = U256::from(gas_used) * U256::from(gas_price);
        let unused_refund = prepaid - spent;
        let guest_post_payload_balance = U256::from(2_885_863_623_750u64);

        let mut db = CacheDB::new(EmptyDB::default());
        // After the synthetic prepayment is consumed, REVM has added its
        // unused-gas refund to the guest-equivalent payload balance.
        set_balance(&mut db, sender, guest_post_payload_balance + unused_refund);
        set_balance(&mut db, coinbase, spent);
        write_tank_slot(&mut db, credit_slot(sender), pre_credit - prepaid).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, pre_credit).unwrap();

        test_plan(sender, coinbase, gas_limit, gas_used, gas_price, gas_price)
            .settle(&mut db)
            .unwrap();

        assert_eq!(
            db.load_account(sender).unwrap().info.balance,
            guest_post_payload_balance
        );
        assert_eq!(db.load_account(coinbase).unwrap().info.balance, U256::ZERO);
        assert_eq!(
            read_tank_slot(&mut db, credit_slot(sender)).unwrap(),
            pre_credit - spent
        );
        assert_eq!(
            read_tank_slot(&mut db, TOTAL_CREDITS_SLOT).unwrap(),
            pre_credit - spent
        );
    }

    #[test]
    fn settlement_composes_with_payload_tank_mutations() {
        let sender = Address::with_last_byte(0x12);
        let coinbase = Address::with_last_byte(0x34);
        let mut db = CacheDB::new(EmptyDB::default());
        set_balance(&mut db, sender, U256::from(100));
        set_balance(&mut db, coinbase, U256::from(40));

        // Post-precharge values after the payload withdrew 50 sender credits
        // and independently credited the coinbase by 50.
        write_tank_slot(&mut db, credit_slot(sender), U256::from(850)).unwrap();
        write_tank_slot(&mut db, credit_slot(coinbase), U256::from(250)).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, U256::from(1_150)).unwrap();

        test_plan(sender, coinbase, 10, 4, 10, 4)
            .settle(&mut db)
            .unwrap();

        assert_eq!(
            read_tank_slot(&mut db, credit_slot(sender)).unwrap(),
            U256::from(910)
        );
        assert_eq!(
            read_tank_slot(&mut db, credit_slot(coinbase)).unwrap(),
            U256::from(274)
        );
        assert_eq!(
            read_tank_slot(&mut db, TOTAL_CREDITS_SLOT).unwrap(),
            U256::from(1_134)
        );
        assert_eq!(
            db.load_account(sender).unwrap().info.balance,
            U256::from(40)
        );
        assert_eq!(db.load_account(coinbase).unwrap().info.balance, U256::ZERO);
    }

    #[test]
    fn sender_coinbase_alias_is_order_independent() {
        let account = Address::with_last_byte(0x77);
        let mut db = CacheDB::new(EmptyDB::default());
        // REVM has credited 60 unused gas plus the 40 beneficiary reward.
        set_balance(&mut db, account, U256::from(100));
        write_tank_slot(&mut db, credit_slot(account), U256::from(900)).unwrap();
        write_tank_slot(&mut db, TOTAL_CREDITS_SLOT, U256::from(1_000)).unwrap();

        test_plan(account, account, 10, 4, 10, 4)
            .settle(&mut db)
            .unwrap();

        assert_eq!(db.load_account(account).unwrap().info.balance, U256::ZERO);
        assert_eq!(
            read_tank_slot(&mut db, credit_slot(account)).unwrap(),
            U256::from(984)
        );
        assert_eq!(
            read_tank_slot(&mut db, TOTAL_CREDITS_SLOT).unwrap(),
            U256::from(984)
        );
    }
}
