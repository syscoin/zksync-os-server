use alloy::primitives::{B256, keccak256};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};
use zksync_os_interface::traits::ReadStorage;
use zksync_os_interface::types::BlockOutput;
use zksync_os_storage_api::BlockContext;
use zksync_os_types::ZkTransaction;

/// [`ReadStorage`] wrapper that tracks read storage slots.
#[derive(Debug)]
pub(super) struct ReadRecordingState<S> {
    inner: S,
    read_keys_handle: ReadRecordingHandle,
}

impl<S: ReadStorage> ReadRecordingState<S> {
    pub(super) fn new(inner: S) -> (Self, ReadRecordingHandle) {
        let handle = ReadRecordingHandle(Rc::default());
        let this = Self {
            inner,
            read_keys_handle: handle.clone(),
        };
        (this, handle)
    }
}

impl<S: ReadStorage> ReadStorage for ReadRecordingState<S> {
    fn read(&mut self, key: B256) -> Option<B256> {
        self.read_keys_handle.record_read(key);
        self.inner.read(key)
    }
}

// SYSCOIN: transaction-scoped read recording prevents rejected L2 txs from
// forcing Merkle proofs for reads outside the sealed block.
#[derive(Debug, Default)]
struct ReadRecorder {
    committed_read_keys: HashSet<B256>,
    current_tx_read_keys: Option<HashSet<B256>>,
}

impl ReadRecorder {
    fn record_read(&mut self, key: B256) {
        if let Some(current_tx_read_keys) = &mut self.current_tx_read_keys {
            current_tx_read_keys.insert(key);
        } else {
            self.committed_read_keys.insert(key);
        }
    }

    fn begin_tx(&mut self) {
        if let Some(read_keys) = self.current_tx_read_keys.take() {
            self.committed_read_keys.extend(read_keys);
        }
        self.current_tx_read_keys = Some(HashSet::new());
    }

    fn finish_tx(&mut self, commit_reads: bool) {
        let Some(read_keys) = self.current_tx_read_keys.take() else {
            return;
        };
        if commit_reads {
            self.committed_read_keys.extend(read_keys);
        }
    }

    fn into_read_keys(mut self) -> HashSet<B256> {
        // Successful blocks should never finish with an active tx read set, but keep
        // any such reads rather than under-proving if the VM callback ordering changes.
        if let Some(read_keys) = self.current_tx_read_keys.take() {
            self.committed_read_keys.extend(read_keys);
        }
        self.committed_read_keys
    }
}

/// Handle for [`ReadRecordingState`] that allows to extract read storage slots after the state is dropped.
#[derive(Clone, Debug)]
pub(super) struct ReadRecordingHandle(Rc<RefCell<ReadRecorder>>);

impl ReadRecordingHandle {
    pub(super) fn begin_tx(&self) {
        self.0.borrow_mut().begin_tx();
    }

    pub(super) fn finish_tx(&self, commit_reads: bool) {
        self.0.borrow_mut().finish_tx(commit_reads);
    }

    fn record_read(&self, key: B256) {
        self.0.borrow_mut().record_read(key);
    }

    pub(super) fn into_read_keys(self) -> HashSet<B256> {
        Rc::try_unwrap(self.0)
            .expect("`into_read_keys()` called while read recorder is still shared")
            .into_inner()
            .into_read_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyStorage;

    impl ReadStorage for EmptyStorage {
        fn read(&mut self, _key: B256) -> Option<B256> {
            None
        }
    }

    #[test]
    fn rejected_tx_reads_are_discarded() {
        let (mut state, handle) = ReadRecordingState::new(EmptyStorage);
        let pre_tx_key = B256::repeat_byte(1);
        let rejected_key = B256::repeat_byte(2);
        let accepted_key = B256::repeat_byte(3);

        state.read(pre_tx_key);
        handle.begin_tx();
        state.read(rejected_key);
        handle.finish_tx(false);
        handle.begin_tx();
        state.read(accepted_key);
        handle.finish_tx(true);
        drop(state);

        let read_keys = handle.into_read_keys();
        assert!(read_keys.contains(&pre_tx_key));
        assert!(read_keys.contains(&accepted_key));
        assert!(!read_keys.contains(&rejected_key));
    }

    #[test]
    fn active_tx_reads_are_kept_on_extract_to_avoid_under_proving() {
        let (mut state, handle) = ReadRecordingState::new(EmptyStorage);
        let active_key = B256::repeat_byte(4);

        handle.begin_tx();
        state.read(active_key);
        drop(state);

        let read_keys = handle.into_read_keys();
        assert!(read_keys.contains(&active_key));
    }

    #[test]
    fn starting_next_tx_commits_unfinished_reads() {
        let (mut state, handle) = ReadRecordingState::new(EmptyStorage);
        let first_key = B256::repeat_byte(5);
        let second_key = B256::repeat_byte(6);

        handle.begin_tx();
        state.read(first_key);
        handle.begin_tx();
        state.read(second_key);
        handle.finish_tx(false);
        drop(state);

        let read_keys = handle.into_read_keys();
        assert!(read_keys.contains(&first_key));
        assert!(!read_keys.contains(&second_key));
    }
}

// Hash of the block output, which is used to identify divergences in block execution.
// It's incomplete, in a sense that it does not include all the data from the block output.
// Hash includes the most important pieces of data that are likely to change in case of a divergence.
pub(crate) fn hash_block_output(block_output: &BlockOutput) -> B256 {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(block_output.header.hash().as_slice());
    for tx in block_output.tx_results.iter().flatten() {
        preimage.extend_from_slice(&[tx.is_success() as u8]);
        preimage.extend_from_slice(&tx.gas_used.to_be_bytes());
    }
    for storage_log in &block_output.storage_writes {
        preimage.extend_from_slice(storage_log.key.as_slice());
        preimage.extend_from_slice(storage_log.value.as_slice());
    }

    keccak256(preimage)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockDump {
    pub ctx: BlockContext,
    pub txs: Vec<ZkTransaction>,
    pub error: String,
}

pub(crate) fn save_dump(path: PathBuf, dump: BlockDump) -> anyhow::Result<()> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Incorrect system time")
        .as_secs();
    let file_name = format!("dump_{}_{seconds}.json", dump.ctx.block_number);
    let bytes = serde_json::to_vec(&dump).context("failed to serialize dump")?;
    std::fs::create_dir_all(&path).context("create_dir_all")?;
    std::fs::write(path.join(file_name), bytes).context("failed to write dump file")?;

    Ok(())
}
