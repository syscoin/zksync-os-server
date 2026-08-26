use alloy::consensus::Transaction;
use alloy::eips::Typed2718;
use alloy::primitives::{Bytes, U256};
use revm::context::TxEnv;
use revm::primitives::TxKind;
use revm::state::Bytecode;
use zk_os_basic_system::system_implementation::flat_storage_model::AccountProperties;
use zksync_os_revm::transaction::abstraction::ZKsyncTxBuilder;
use zksync_os_revm::{ZKsyncTx, ZkSpecId};
use zksync_os_types::{ExecutionVersion, ZkTransaction};

/// Get unpadded code from full bytecode with artifacts.
pub fn get_unpadded_code(full_bytecode: &[u8], account: &AccountProperties) -> Bytecode {
    Bytecode::new_legacy(Bytes::copy_from_slice(
        &full_bytecode[0..account.unpadded_code_len as usize],
    ))
}

/// Convert a ZkTransaction into a revm TxEnv for REVM re-execution.
///
/// `block_gas_limit` is used for system txs, whose own `gas_limit` is 0;
/// the new revm rejects the tx if `gas_used_override` exceeds `gas_limit`.
pub fn zk_tx_into_revm_tx(
    tx: &ZkTransaction,
    gas_used: u64,
    execution_status: bool,
    block_gas_limit: u64,
    settlement_layer_chain_id: Option<U256>,
) -> anyhow::Result<ZKsyncTx<TxEnv>> {
    let caller = tx.signer();

    let envelope = tx.envelope();

    let mut blob_hashes = vec![];
    let mut max_fee_per_blob_gas = 0;
    let mut authorization_list = vec![];

    let (
        gas_price,
        gas_priority_fee,
        value,
        data,
        chain_id,
        access_list,
        to_mint,
        refund_recipient,
        gas_limit,
    ) = match envelope {
        zksync_os_types::ZkEnvelope::System(system_tx) => (
            0,
            Some(0),
            U256::ZERO,
            system_tx.input().clone(),
            None,
            Default::default(),
            Default::default(),
            None,
            block_gas_limit,
        ),
        zksync_os_types::ZkEnvelope::L2(l2_tx) => {
            let gas_price = l2_tx.max_fee_per_gas();
            let priority_fee = l2_tx.max_priority_fee_per_gas();
            let value = l2_tx.value();
            let data = l2_tx.input().clone();
            let chain_id = l2_tx.chain_id();
            let access_list = l2_tx.access_list().cloned().unwrap_or_default();
            blob_hashes = l2_tx
                .blob_versioned_hashes()
                .map(|hashes| hashes.to_vec())
                .unwrap_or_default();
            max_fee_per_blob_gas = l2_tx.max_fee_per_blob_gas().unwrap_or_default();
            authorization_list = l2_tx
                .authorization_list()
                .map(|list| list.to_vec())
                .unwrap_or_default();

            (
                gas_price,
                priority_fee,
                value,
                data,
                chain_id,
                access_list,
                Default::default(),
                None,
                tx.gas_limit(),
            )
        }
        zksync_os_types::ZkEnvelope::L1(l1_tx) => {
            let inner = &l1_tx.inner;
            (
                l1_tx.max_fee_per_gas(),
                l1_tx.max_priority_fee_per_gas(),
                inner.value(),
                inner.input().clone(),
                None,
                Default::default(),
                inner.to_mint,
                Some(inner.refund_recipient),
                tx.gas_limit(),
            )
        }
        zksync_os_types::ZkEnvelope::Upgrade(upgrade_tx) => {
            let inner = &upgrade_tx.inner;
            (
                0,
                None,
                inner.value(),
                inner.input().clone(),
                None,
                Default::default(),
                upgrade_tx.inner.to_mint,
                Some(inner.refund_recipient),
                tx.gas_limit(),
            )
        }
    };

    let transact_to = match tx.to() {
        Some(to) => TxKind::Call(to),
        None => TxKind::Create,
    };

    // SYSCOIN: A service tx's envelope nonce is a uniqueness salt, not an account nonce. Passing
    // the fresh-chain placeholder salt (`u64::MAX`) through TxEnv makes REVM reject it during
    // environment validation before its service-tx path can skip account nonce semantics.
    let revm_nonce = if matches!(envelope, zksync_os_types::ZkEnvelope::System(_)) {
        0
    } else {
        tx.nonce()
    };
    let mut tx_env_builder = TxEnv::builder()
        .caller(caller)
        .gas_limit(gas_limit)
        .gas_price(gas_price)
        .kind(transact_to)
        .value(value)
        .data(data)
        .nonce(revm_nonce)
        .access_list(access_list)
        .tx_type(Some(tx.tx_type().ty()))
        .chain_id(chain_id)
        .blob_hashes(blob_hashes)
        .max_fee_per_blob_gas(max_fee_per_blob_gas)
        .authorization_list_signed(authorization_list);

    if let Some(priority_fee) = gas_priority_fee {
        tx_env_builder = tx_env_builder.gas_priority_fee(Some(priority_fee));
    }

    ZKsyncTxBuilder::new()
        .base(tx_env_builder)
        .mint(to_mint)
        .refund_recipient(refund_recipient)
        .settlement_layer_chain_id(settlement_layer_chain_id)
        .gas_used_override(Some(gas_used))
        .force_fail(!execution_status)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build TxEnv: {e:?}"))
}

pub fn zk_spec_version(execution_version: ExecutionVersion) -> Option<ZkSpecId> {
    // SYSCOIN: The fresh V32 deployment accepts only the pinned V7 / AtlasV3 execution surface;
    // pre-mainnet legacy execution versions are deliberately not retained as live mappings.
    match execution_version {
        ExecutionVersion::V7 => Some(ZkSpecId::AtlasV3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, B256};
    use revm::ExecuteCommitEvm;
    use revm::context_interface::Transaction as RevmTransaction;
    use revm::database::{CacheDB, EmptyDB};
    use revm::state::AccountInfo;
    use zksync_os_revm::transaction::abstraction::ZkTxTr;
    use zksync_os_revm::{DefaultZk, ZkBuilder, ZkContext};
    use zksync_os_types::{
        L1Tx, L1UpgradeEnvelope, SystemTxEnvelope, UpgradeTxType, ZkTransaction,
    };

    #[test]
    fn upgrade_then_max_salt_system_tx_executes_in_revm() {
        let upgrade: ZkTransaction = L1UpgradeEnvelope {
            inner: L1Tx::<UpgradeTxType> {
                hash: B256::ZERO,
                initiator: Address::from_word(B256::with_last_byte(0x07)),
                to: Address::from_word(B256::with_last_byte(0x0f)),
                gas_limit: 72_000_000,
                gas_per_pubdata_byte_limit: 800,
                max_fee_per_gas: 0,
                max_priority_fee_per_gas: 0,
                nonce: 32,
                value: U256::ZERO,
                to_mint: U256::ZERO,
                refund_recipient: Address::ZERO,
                input: Bytes::new(),
                factory_deps: vec![],
                marker: std::marker::PhantomData,
            },
        }
        .into();
        let placeholder: ZkTransaction = SystemTxEnvelope::set_sl_chain_id(31_337, u64::MAX).into();
        assert_eq!(placeholder.nonce(), u64::MAX);

        let upgrade = zk_tx_into_revm_tx(&upgrade, 0, true, 72_000_000, None).unwrap();
        let placeholder = zk_tx_into_revm_tx(&placeholder, 0, true, 72_000_000, None).unwrap();

        assert_eq!(upgrade.tx_type(), 0x7e);
        assert!(upgrade.is_l1_to_l2_tx());
        assert_eq!(placeholder.tx_type(), 0x7d);
        assert_eq!(placeholder.nonce(), 0);
        assert!(!placeholder.service_tx);
        assert!(placeholder.is_service_tx());

        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            upgrade.caller(),
            AccountInfo {
                nonce: u64::MAX,
                ..Default::default()
            },
        );
        db.insert_account_info(
            placeholder.caller(),
            AccountInfo {
                nonce: u64::MAX,
                ..Default::default()
            },
        );
        let mut evm = ZkContext::<EmptyDB>::default()
            .with_db(db)
            .modify_cfg_chained(|cfg| cfg.spec = ZkSpecId::AtlasV3)
            .build_zk();
        evm.transact_commit(upgrade)
            .expect("upgrade tx must bypass account nonce validation");
        evm.transact_commit(placeholder)
            .expect("service tx must bypass account nonce validation");
    }
}
