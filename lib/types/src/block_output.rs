use alloy::consensus::{Header, Sealed};
use alloy::primitives::B256;
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::types::{AccountDiff, StorageWrite, TxOutput};

/// SYSCOIN: Fresh V32 execution retains only pubdata usage; legacy full-byte output is unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPubdata(u64);

impl BlockPubdata {
    pub const fn new(used: u64) -> Self {
        Self(used)
    }

    pub const fn used(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct BlockOutput {
    pub header: Sealed<Header>,
    pub tx_results: Vec<Result<TxOutput, InvalidTransaction>>,
    pub storage_writes: Vec<StorageWrite>,
    pub account_diffs: Vec<AccountDiff>,
    pub published_preimages: Vec<(B256, Vec<u8>)>,
    pub pubdata: BlockPubdata,
    pub computational_native_used: u64,
}

impl BlockOutput {
    pub fn pubdata_used(&self) -> u64 {
        self.pubdata.used()
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockOutput, BlockPubdata};
    use alloy::consensus::{Header, Sealable};

    fn block_output(pubdata: BlockPubdata) -> BlockOutput {
        BlockOutput {
            header: Header::default().seal_slow(),
            tx_results: vec![],
            storage_writes: vec![],
            account_diffs: vec![],
            published_preimages: vec![],
            pubdata,
            computational_native_used: 0,
        }
    }

    #[test]
    fn pubdata_reports_usage() {
        let output = block_output(BlockPubdata::new(7));
        assert_eq!(output.pubdata_used(), 7);
    }
}
