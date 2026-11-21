use std::collections::HashMap;

use crate::ViewState;
use alloy::primitives::ruint::aliases::B160;
use alloy::primitives::{Address, B256};
use alloy::rpc::types::state::StateOverride;
use zk_ee::common_structs::derive_flat_storage_key;
use zk_os_api::helpers::{set_properties_balance, set_properties_code, set_properties_nonce};
use zk_os_basic_system::system_implementation::flat_storage_model::{
    ACCOUNT_PROPERTIES_STORAGE_ADDRESS, AccountProperties, address_into_special_storage_key,
};
use zksync_os_interface::traits::{PreimageSource, ReadStorage};

#[derive(Debug, thiserror::Error)]
pub enum StateOverrideError {
    #[error(
        "full storage replacement via `state` is unsupported. Use `stateDiff` for sparse overrides"
    )]
    FullStateOverrideUnsupported,
}

/// Trait for providing storage and preimage overrides.
/// Allows different implementations: owned HashMaps for RPC calls, or shared data for sequencer.
/// Requires 'static because it's used in types that implement ReadStorage/PreimageSource.
pub trait OverrideProvider: 'static {
    /// Look up a storage override by key.
    fn get_storage_override(&self, key: &B256) -> Option<B256>;

    /// Look up a preimage override by hash.
    fn get_preimage_override(&self, hash: &B256) -> Option<Vec<u8>>;
}

/// Owned HashMap-based override provider.
/// Used for RPC calls with StateOverride, where we own the override data.
#[derive(Debug, Clone, Default)]
pub struct OwnedOverrides {
    storage: HashMap<B256, B256>,
    preimages: HashMap<B256, Vec<u8>>,
}

impl OwnedOverrides {
    pub fn new(storage: HashMap<B256, B256>, preimages: HashMap<B256, Vec<u8>>) -> Self {
        Self { storage, preimages }
    }

    pub fn extend(&mut self, changes: Self) {
        self.storage.extend(changes.storage);
        self.preimages.extend(changes.preimages);
    }

    // SYSCOIN: used by RPC simulation to keep internal fee-bypass balance overrides
    // from leaking into the cross-block simulation overlay.
    pub fn remove_storage_override(&mut self, key: &B256) {
        self.storage.remove(key);
    }

    // SYSCOIN: lets RPC simulation rewrite internal account-property overlays
    // without exposing the whole override map.
    pub fn insert_storage_override(&mut self, key: B256, value: B256) {
        self.storage.insert(key, value);
    }

    // SYSCOIN: lets RPC simulation publish rewritten account-property preimages
    // without exposing the whole override map.
    pub fn insert_preimage_override(&mut self, hash: B256, preimage: Vec<u8>) {
        self.preimages.insert(hash, preimage);
    }
}

impl OverrideProvider for OwnedOverrides {
    fn get_storage_override(&self, key: &B256) -> Option<B256> {
        self.storage.get(key).copied()
    }

    fn get_preimage_override(&self, hash: &B256) -> Option<Vec<u8>> {
        self.preimages.get(hash).cloned()
    }
}

/// A `ViewState` wrapper that overrides specific storage slots.
/// All other reads/preimage lookups delegate to the inner state.
/// Generic over both the inner state V and the override provider O.
#[derive(Debug, Clone)]
pub struct OverriddenStateView<V: ViewState, O: OverrideProvider> {
    inner: V,
    overrides: O,
}

impl<V: ViewState, O: OverrideProvider> OverriddenStateView<V, O> {
    pub fn new(inner: V, overrides: O) -> Self {
        Self { inner, overrides }
    }
}

// Convenience constructors for common cases
impl<V: ViewState> OverriddenStateView<V, OwnedOverrides> {
    /// Create from RPC StateOverride.
    pub fn with_state_overrides(
        inner: V,
        state_overrides: StateOverride,
    ) -> Result<Self, StateOverrideError> {
        let overrides = build_state_override_maps(&inner, state_overrides)?;
        Ok(Self::new(inner, overrides))
    }

    /// Create with only preimage overrides.
    pub fn with_preimages(inner: V, preimage_overrides: &[(B256, Vec<u8>)]) -> Self {
        let preimages = preimage_overrides
            .iter()
            .cloned()
            .collect::<HashMap<B256, Vec<u8>>>();
        Self::new(inner, OwnedOverrides::new(HashMap::new(), preimages))
    }
}

impl<V: ViewState, O: OverrideProvider> ReadStorage for OverriddenStateView<V, O> {
    fn read(&mut self, key: B256) -> Option<B256> {
        if let Some(val) = self.overrides.get_storage_override(&key) {
            return Some(val);
        }

        self.inner.read(key)
    }
}

impl<V: ViewState, O: OverrideProvider> PreimageSource for OverriddenStateView<V, O> {
    fn get_preimage(&mut self, hash: B256) -> Option<Vec<u8>> {
        if let Some(bytes) = self.overrides.get_preimage_override(&hash) {
            return Some(bytes);
        }

        self.inner.get_preimage(hash)
    }
}

/// Converts RPC `StateOverride` into an `OwnedOverrides` provider.
pub fn build_state_override_maps<V: ViewState>(
    inner: &V,
    state_overrides: StateOverride,
) -> Result<OwnedOverrides, StateOverrideError> {
    let mut storage: HashMap<B256, B256> = HashMap::new();
    let mut preimages: HashMap<B256, Vec<u8>> = HashMap::new();

    // `StateOverride` is a map-like structure of Address => AccountOverride
    for (address, account) in state_overrides {
        if account.state.is_some() {
            // Flat storage reads receive a hash of the address and slot, so this view
            // cannot zero omitted slots for a specific account.
            return Err(StateOverrideError::FullStateOverrideUnsupported);
        }

        if let Some(state_diff) = account.state_diff {
            for (slot, value_override) in state_diff {
                let flat_key = derive_flat_storage_key(
                    &B160::from_be_bytes(address.into_array()),
                    &(slot.0.into()),
                );
                storage.insert(B256::from(flat_key.as_u8_array()), value_override);
            }
        }

        if account.balance.is_some() || account.nonce.is_some() || account.code.is_some() {
            // start from current account props if present
            let mut base: AccountProperties =
                inner.clone().get_account(address).unwrap_or_default();

            if let Some(nonce) = account.nonce {
                set_properties_nonce(&mut base, nonce);
            }
            if let Some(balance) = account.balance {
                set_properties_balance(&mut base, balance);
            }
            if let Some(code) = account.code {
                let bytecode_preimage = set_properties_code(&mut base, &code);
                let bytecode_hash_b256: B256 = base.bytecode_hash.as_u8_array().into();
                preimages.insert(bytecode_hash_b256, bytecode_preimage);
            }

            // Compute and store account properties preimage and its hash
            let acc_hash = base.compute_hash();
            let acc_hash_b256: B256 = acc_hash.as_u8_array().into();
            preimages.insert(acc_hash_b256, base.encoding().to_vec());

            // Compute flat storage key for account properties of this address and override it
            storage.insert(account_properties_storage_key(address), acc_hash_b256);
        }
    }

    Ok(OwnedOverrides::new(storage, preimages))
}

// SYSCOIN: exposes the account-properties slot key so callers that create internal
// balance overrides can remove only those synthetic account writes from overlays.
pub fn account_properties_storage_key(address: Address) -> B256 {
    let key = derive_flat_storage_key(
        &ACCOUNT_PROPERTIES_STORAGE_ADDRESS,
        &address_into_special_storage_key(&B160::from_be_bytes(address.into_array())),
    );
    B256::from(key.as_u8_array())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::rpc::types::state::AccountOverride;

    #[derive(Debug, Clone, Default)]
    struct MockState {
        storage: HashMap<B256, B256>,
        preimages: HashMap<B256, Vec<u8>>,
    }

    impl ReadStorage for MockState {
        fn read(&mut self, key: B256) -> Option<B256> {
            self.storage.get(&key).copied()
        }
    }

    impl PreimageSource for MockState {
        fn get_preimage(&mut self, hash: B256) -> Option<Vec<u8>> {
            self.preimages.get(&hash).cloned()
        }
    }

    fn flat_key(address: Address, slot: B256) -> B256 {
        let flat_key =
            derive_flat_storage_key(&B160::from_be_bytes(address.into_array()), &(slot.0.into()));
        B256::from(flat_key.as_u8_array())
    }

    #[test]
    fn state_diff_overrides_sparse_slots_and_falls_back_for_others() {
        let address = Address::from([0x11; 20]);
        let overridden_slot = B256::from([0x22; 32]);
        let fallback_slot = B256::from([0x33; 32]);
        let base_value = B256::from([0x44; 32]);
        let override_value = B256::from([0x55; 32]);
        let fallback_value = B256::from([0x66; 32]);

        let state = MockState {
            storage: HashMap::from([
                (flat_key(address, overridden_slot), base_value),
                (flat_key(address, fallback_slot), fallback_value),
            ]),
            preimages: HashMap::new(),
        };
        let overrides = StateOverride::from_iter([(
            address,
            AccountOverride::default().with_state_diff([(overridden_slot, override_value)]),
        )]);

        let mut view =
            OverriddenStateView::with_state_overrides(state, overrides).expect("stateDiff works");

        assert_eq!(
            view.read(flat_key(address, overridden_slot)),
            Some(override_value)
        );
        assert_eq!(
            view.read(flat_key(address, fallback_slot)),
            Some(fallback_value)
        );
    }

    #[test]
    fn full_state_override_is_rejected_instead_of_treated_as_sparse_diff() {
        let address = Address::from([0x11; 20]);
        let slot = B256::from([0x22; 32]);
        let value = B256::from([0x55; 32]);
        for account in [
            AccountOverride::default().with_state([]),
            AccountOverride::default().with_state([(slot, value)]),
        ] {
            let overrides = StateOverride::from_iter([(address, account)]);
            let result = OverriddenStateView::with_state_overrides(MockState::default(), overrides);
            assert!(matches!(
                result,
                Err(StateOverrideError::FullStateOverrideUnsupported)
            ));
        }
    }
}
