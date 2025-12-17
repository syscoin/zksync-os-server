use crate::{ZkTransaction, transaction::system::envelope::SystemTransactionEnvelope};
use serde::{Deserialize, Serialize};

pub mod envelope;
pub mod tx;

pub type InteropRootsEnvelope = SystemTransactionEnvelope<InteropRootsTxType>;

pub trait SystemTxType: Clone + Send + Sync + std::fmt::Debug + 'static {
    const TX_TYPE: u8;
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct InteropRootsTxType;

impl SystemTxType for InteropRootsTxType {
    const TX_TYPE: u8 = 0x7d;
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct InteropRootsTransaction {
    pub interop_roots_amount: u64,
    pub tx: InteropRootsEnvelope,
}

impl From<InteropRootsTransaction> for ZkTransaction {
    fn from(value: InteropRootsTransaction) -> Self {
        ZkTransaction::from(value.tx)
    }
}
