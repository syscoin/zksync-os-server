use alloy::consensus::{Header, Sealed};
use alloy::primitives::{B256, keccak256};
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

/// SYSCOIN: The existing replay-output hash, shared with native batch validation without changing
/// its persisted encoding. This diagnostic binds the header, accepted tx status/gas, and writes;
/// it deliberately does not hash every output field or native-mode resource counter.
pub fn block_output_hash(
    header_hash: B256,
    tx_results: &[Result<TxOutput, InvalidTransaction>],
    storage_writes: &[StorageWrite],
) -> B256 {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(header_hash.as_slice());
    for tx in tx_results.iter().flatten() {
        preimage.extend_from_slice(&[tx.is_success() as u8]);
        preimage.extend_from_slice(&tx.gas_used.to_be_bytes());
    }
    for storage_log in storage_writes {
        preimage.extend_from_slice(storage_log.key.as_slice());
        preimage.extend_from_slice(storage_log.value.as_slice());
    }
    keccak256(preimage)
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
