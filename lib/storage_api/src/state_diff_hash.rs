// SYSCOIN: compute the short state-diff hash header used by Bitcoin DA commitments.
use crate::{ReadStateHistory, ReplayRecord, StateResult, ViewState};
use alloy::primitives::ruint::aliases::B160;
use alloy::primitives::B256;
use blake2::{Blake2s256, Digest};
use zk_ee::common_structs::{WarmStorageKey, derive_flat_storage_key};
use zk_ee::utils::Bytes32;
use zk_os_basic_system::system_implementation::flat_storage_model::{
    ACCOUNT_PROPERTIES_STORAGE_ADDRESS, AccountProperties, address_into_special_storage_key,
};
use zksync_os_interface::types::BlockOutput;

pub fn calculate_state_diffs_hash<'a, ReadState, I>(
    blocks: I,
    read_state: &ReadState,
) -> StateResult<B256>
where
    ReadState: ReadStateHistory,
    I: IntoIterator<Item = (&'a BlockOutput, &'a ReplayRecord)>,
{
    let mut state_diffs_hasher = Blake2s256::new();

    for (block_output, replay_record) in blocks {
        let mut state_view = read_state.state_view_at(replay_record.block_context.block_number)?;
        let mut state_diffs: Vec<(WarmStorageKey, Bytes32)> = Vec::new();

        for storage_write in &block_output.storage_writes {
            state_diffs.push((
                WarmStorageKey {
                    address: B160::from_be_bytes(storage_write.account.into_array()),
                    key: Bytes32::from_array(storage_write.account_key.0),
                },
                Bytes32::from_array(storage_write.value.0),
            ));
        }

        for account_diff in &block_output.account_diffs {
            let account_address = B160::from_be_bytes(account_diff.address.into_array());
            let account_key = address_into_special_storage_key(&account_address);
            let account_properties = state_view
                .get_account(account_diff.address)
                .unwrap_or(AccountProperties::TRIVIAL_VALUE);
            state_diffs.push((
                WarmStorageKey {
                    address: ACCOUNT_PROPERTIES_STORAGE_ADDRESS,
                    key: account_key,
                },
                account_properties.compute_hash(),
            ));
        }

        state_diffs.sort_by_key(|(key, _)| *key);

        for (key, value) in state_diffs {
            let derived_key = derive_flat_storage_key(&key.address, &key.key);
            state_diffs_hasher.update(derived_key.as_u8_ref());
            state_diffs_hasher.update(value.as_u8_ref());
        }
    }

    Ok(B256::from_slice(&state_diffs_hasher.finalize()))
}
