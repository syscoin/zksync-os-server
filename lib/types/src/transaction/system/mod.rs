use crate::transaction::system::envelope::SystemTransactionEnvelope;
use crate::transaction::system::tx::SystemTransaction;
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
