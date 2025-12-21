use std::marker::PhantomData;

use crate::transaction::{system::envelope::SystemTransactionEnvelope, tx::SystemTransaction};
use alloy::primitives::{Address, Bytes, address};
use alloy::sol_types::SolCall;
use serde::{Deserialize, Serialize};
use zksync_os_contract_interface::{InteropRoot, addInteropRootsInBatchCall};

pub mod envelope;
pub mod tx;

pub const BOOTLOADER_FORMAL_ADDRESS: Address =
    address!("0x0000000000000000000000000000000000008001");
pub const L2_INTEROP_ROOT_STORAGE_ZKSYNC_OS_ADDRESS: Address =
    address!("0x0000000000000000000000000000000000010008");

pub type InteropRootsEnvelope = SystemTransactionEnvelope<InteropRootsTxType>;

impl InteropRootsEnvelope {
    pub fn from_interop_roots(interop_roots: Vec<InteropRoot>) -> Self {
        let calldata = addInteropRootsInBatchCall {
            interopRootsInput: interop_roots,
        }
        .abi_encode();

        Self {
            inner: SystemTransaction {
                gas_limit: 0,
                to: L2_INTEROP_ROOT_STORAGE_ZKSYNC_OS_ADDRESS,
                input: Bytes::from(calldata),
                marker: PhantomData,
            },
        }
    }

    pub fn interop_roots_count(&self) -> u64 {
        let interop_roots = addInteropRootsInBatchCall::abi_decode(&self.inner.input)
            .expect("Failed to decode interop roots calldata")
            .interopRootsInput;
        interop_roots.len() as u64
    }
}

pub trait SystemTxType: Clone + Send + Sync + std::fmt::Debug + 'static {
    const TX_TYPE: u8;
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct InteropRootsTxType;

impl SystemTxType for InteropRootsTxType {
    const TX_TYPE: u8 = 0x7d;
}
