use crate::{
    Database, DefaultTreeParams, HashTree, MerkleTree, RocksDBWrapper, TreeParams, leaf_nibbles,
    types::{KeyLookup, Node, NodeKey},
};
use alloy::primitives::{B256, FixedBytes};
use std::{
    fmt::Debug,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};
use zk_ee::utils::Bytes32;
use zk_ee_dev::utils::Bytes32 as Bytes32Dev;
use zk_os_basic_system::system_implementation::flat_storage_model::FlatStorageLeaf;
use zk_os_basic_system_dev::system_implementation::flat_storage_model::FlatStorageLeaf as FlatStorageLeafDev;
use zk_os_forward_system::run::{LeafProof, ReadStorage, ReadStorageTree};
use zk_os_forward_system_dev::run::{
    LeafProof as LeafProofDev, ReadStorage as ReadStorageDev, ReadStorageTree as ReadStorageTreeDev,
};
use zksync_os_merkle_tree_api::Leaf;

type ReadStorageTreeDevLogFile = Arc<Mutex<File>>;

pub struct MerkleTreeVersion<DB: Database = RocksDBWrapper, P: TreeParams = DefaultTreeParams> {
    pub tree: MerkleTree<DB, P>,
    pub block: u64,
    read_storage_tree_dev_log_file: Option<ReadStorageTreeDevLogFile>,
}

impl<DB: Database, P: TreeParams> MerkleTreeVersion<DB, P> {
    pub fn new(tree: MerkleTree<DB, P>, block: u64) -> Self {
        Self {
            tree,
            block,
            read_storage_tree_dev_log_file: None,
        }
    }

    pub fn set_read_storage_tree_dev_log_file(&mut self, file: File) {
        self.read_storage_tree_dev_log_file = Some(Arc::new(Mutex::new(file)));
    }

    pub fn set_read_storage_tree_dev_log_path(
        &mut self,
        path: impl AsRef<Path>,
    ) -> std::io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.set_read_storage_tree_dev_log_file(file);
        Ok(())
    }

    pub fn clear_read_storage_tree_dev_log_file(&mut self) {
        self.read_storage_tree_dev_log_file = None;
    }

    pub fn root_info(&self) -> Result<(B256, u64), anyhow::Error> {
        // We know that the root exists, as some version was loaded into the tree already.
        self.tree.root_info(self.block).transpose().unwrap()
    }

    fn traverse_to_leaf(&mut self, tree_index: u64) -> Option<Leaf> {
        let mut current_node = self
            .tree
            .db()
            .try_root(self.block)
            .unwrap()
            .unwrap()
            .root_node;

        let mut nibble_count = 1;
        loop {
            let index_on_level =
                tree_index >> ((leaf_nibbles::<P>() - nibble_count) * P::INTERNAL_NODE_DEPTH);
            let child_index = index_on_level as usize % (1 << P::INTERNAL_NODE_DEPTH);

            let Some(child) = current_node.children.get(child_index) else {
                break None;
            };
            current_node = match self
                .tree
                .db
                .try_nodes(&[NodeKey {
                    version: child.version,
                    nibble_count,
                    index_on_level,
                }])
                .expect("inconsistent child reference")[0]
                .clone()
            {
                Node::Internal(internal) => internal,
                Node::Leaf(leaf) => break Some(leaf),
            };
            nibble_count += 1;
        }
    }

    fn log_read_storage_tree_dev_call<Input: Debug, Output: Debug>(
        &self,
        method: &str,
        input: &Input,
        output: &Output,
    ) {
        write_read_storage_tree_dev_log(
            &self.read_storage_tree_dev_log_file,
            self.block,
            method,
            input,
            output,
        );
    }
}

fn write_read_storage_tree_dev_log<Input: Debug, Output: Debug>(
    file: &Option<ReadStorageTreeDevLogFile>,
    block: u64,
    method: &str,
    input: &Input,
    output: &Output,
) {
    let Some(file) = file else {
        return;
    };
    let Ok(mut file) = file.lock() else {
        return;
    };

    let _ = writeln!(
        file,
        "block={} method={} input={input:?} output={output:?}",
        block, method
    );
    let _ = file.flush();
}

impl<DB: Database + 'static, P: TreeParams + 'static> ReadStorage for MerkleTreeVersion<DB, P> {
    fn read(&mut self, key: Bytes32) -> Option<Bytes32> {
        <Self as ReadStorageTree>::tree_index(self, key).and_then(|index| {
            self.traverse_to_leaf(index)
                .map(|Leaf { value, .. }| fixed_bytes_to_bytes32(value))
        })
    }
}

impl<DB: Database + 'static, P: TreeParams + 'static> ReadStorageTree for MerkleTreeVersion<DB, P> {
    fn tree_index(&mut self, key: Bytes32) -> Option<u64> {
        self.tree
            .db()
            .indices(self.block, &[FixedBytes::from_slice(key.as_u8_ref())])
            .ok()
            .and_then(|v| match v[0] {
                KeyLookup::Existing(x) => Some(x),
                KeyLookup::Missing { .. } => None,
            })
    }

    fn merkle_proof(&mut self, tree_index: u64) -> LeafProof {
        let mut sibling_hashes = Box::new([Bytes32::zero(); 64]);

        let mut current_node = self
            .tree
            .db()
            .try_root(self.block)
            .unwrap()
            .unwrap()
            .root_node;

        let mut i = P::TREE_DEPTH as usize;
        let mut nibble_count = 1;
        let leaf = loop {
            let index_on_level =
                tree_index >> ((leaf_nibbles::<P>() - nibble_count) * P::INTERNAL_NODE_DEPTH);
            let child_index = index_on_level as usize % (1 << P::INTERNAL_NODE_DEPTH);

            // the root does not contain any nodes apart from its children
            if nibble_count > 1 {
                let hashes = current_node
                    .internal_hashes::<P>(&self.tree.hasher, i as u8 - 3)
                    .0;

                for depth in 0..P::INTERNAL_NODE_DEPTH - 1 {
                    let needed_for_this_and_lower_levels = (2 << (depth + 1)) - 2;
                    let needed_for_all = (2 << (P::INTERNAL_NODE_DEPTH - 1)) - 2;
                    let skip = needed_for_all - needed_for_this_and_lower_levels;

                    let index = child_index >> (P::INTERNAL_NODE_DEPTH - depth - 1);

                    i -= 1;
                    sibling_hashes[i] = hashes[skip + (index ^ 1)].0.into();
                }
            }

            i -= 1;
            sibling_hashes[i] = current_node
                .children
                .get(child_index ^ 1)
                .map(|x| x.hash)
                .unwrap_or(self.tree.hasher.empty_subtree_hash(i as u8))
                .0
                .into();

            let Some(child) = current_node.children.get(child_index) else {
                break Leaf::default();
            };
            current_node = match self
                .tree
                .db
                .try_nodes(&[NodeKey {
                    version: child.version,
                    nibble_count,
                    index_on_level,
                }])
                .expect("inconsistent child reference")[0]
                .clone()
            {
                Node::Internal(internal) => internal,
                Node::Leaf(leaf) => break leaf,
            };
            nibble_count += 1;
        };

        for i in 0..i {
            sibling_hashes[i] = self.tree.hasher.empty_subtree_hash(i as u8).0.into();
        }

        LeafProof::new(
            tree_index,
            FlatStorageLeaf {
                key: leaf.key.0.into(),
                value: leaf.value.0.into(),
                next: leaf.next_index,
            },
            sibling_hashes,
        )
    }

    fn prev_tree_index(&mut self, key: Bytes32) -> u64 {
        // TODO: callers only invoke this for keys that are absent (membership proofs for missing
        // keys). If the key already exists in the tree, a different code path is needed.
        let res = &self
            .tree
            .db()
            .indices(self.block, &[FixedBytes::from_slice(key.as_u8_ref())])
            .unwrap()[0];
        match res {
            KeyLookup::Existing(_) => todo!("prev_tree_index for existing keys is not implemented"),
            KeyLookup::Missing {
                prev_key_and_index: (_, index),
                ..
            } => *index,
        }
    }
}

impl<DB: Database + 'static, P: TreeParams + 'static> ReadStorageDev for MerkleTreeVersion<DB, P> {
    fn read(&mut self, key: Bytes32Dev) -> Option<Bytes32Dev> {
        <Self as ReadStorageTreeDev>::tree_index(self, key).and_then(|index| {
            self.traverse_to_leaf(index)
                .map(|Leaf { value, .. }| fixed_bytes_to_bytes32_dev(value))
        })
    }
}

impl<DB: Database + 'static, P: TreeParams + 'static> ReadStorageTreeDev
    for MerkleTreeVersion<DB, P>
{
    fn tree_index(&mut self, key: Bytes32Dev) -> Option<u64> {
        let result = self
            .tree
            .db()
            .indices(self.block, &[FixedBytes::from_slice(key.as_u8_ref())])
            .ok()
            .and_then(|v| match v[0] {
                KeyLookup::Existing(x) => Some(x),
                KeyLookup::Missing { .. } => None,
            });
        self.log_read_storage_tree_dev_call("tree_index", &key, &result);
        result
    }

    fn merkle_proof(&mut self, tree_index: u64) -> LeafProofDev {
        let mut sibling_hashes = Box::new([Bytes32Dev::zero(); 64]);

        let mut current_node = self
            .tree
            .db()
            .try_root(self.block)
            .unwrap()
            .unwrap()
            .root_node;

        let mut i = P::TREE_DEPTH as usize;
        let mut nibble_count = 1;
        let leaf = loop {
            let index_on_level =
                tree_index >> ((leaf_nibbles::<P>() - nibble_count) * P::INTERNAL_NODE_DEPTH);
            let child_index = index_on_level as usize % (1 << P::INTERNAL_NODE_DEPTH);

            // the root does not contain any nodes apart from its children
            if nibble_count > 1 {
                let hashes = current_node
                    .internal_hashes::<P>(&self.tree.hasher, i as u8 - 3)
                    .0;

                for depth in 0..P::INTERNAL_NODE_DEPTH - 1 {
                    let needed_for_this_and_lower_levels = (2 << (depth + 1)) - 2;
                    let needed_for_all = (2 << (P::INTERNAL_NODE_DEPTH - 1)) - 2;
                    let skip = needed_for_all - needed_for_this_and_lower_levels;

                    let index = child_index >> (P::INTERNAL_NODE_DEPTH - depth - 1);

                    i -= 1;
                    sibling_hashes[i] = hashes[skip + (index ^ 1)].0.into();
                }
            }

            i -= 1;
            sibling_hashes[i] = current_node
                .children
                .get(child_index ^ 1)
                .map(|x| x.hash)
                .unwrap_or(self.tree.hasher.empty_subtree_hash(i as u8))
                .0
                .into();

            let Some(child) = current_node.children.get(child_index) else {
                break Leaf::default();
            };
            current_node = match self
                .tree
                .db
                .try_nodes(&[NodeKey {
                    version: child.version,
                    nibble_count,
                    index_on_level,
                }])
                .expect("inconsistent child reference")[0]
                .clone()
            {
                Node::Internal(internal) => internal,
                Node::Leaf(leaf) => break leaf,
            };
            nibble_count += 1;
        };

        for i in 0..i {
            sibling_hashes[i] = self.tree.hasher.empty_subtree_hash(i as u8).0.into();
        }

        let result = LeafProofDev::new(
            tree_index,
            FlatStorageLeafDev {
                key: leaf.key.0.into(),
                value: leaf.value.0.into(),
                next: leaf.next_index,
            },
            sibling_hashes,
        );
        self.log_read_storage_tree_dev_call("merkle_proof", &tree_index, &result);
        result
    }

    fn prev_tree_index(&mut self, key: Bytes32Dev) -> u64 {
        // TODO: callers only invoke this for keys that are absent (membership proofs for missing
        // keys). If the key already exists in the tree, a different code path is needed.
        let res = &self
            .tree
            .db()
            .indices(self.block, &[FixedBytes::from_slice(key.as_u8_ref())])
            .unwrap()[0];
        let result = match res {
            KeyLookup::Existing(_) => todo!("prev_tree_index for existing keys is not implemented"),
            KeyLookup::Missing {
                prev_key_and_index: (_, index),
                ..
            } => *index,
        };
        self.log_read_storage_tree_dev_call("prev_tree_index", &key, &result);
        result
    }
}

pub fn fixed_bytes_to_bytes32(x: B256) -> Bytes32 {
    let x: [u8; 32] = x.into();
    x.into()
}

pub fn fixed_bytes_to_bytes32_dev(x: B256) -> Bytes32Dev {
    let x: [u8; 32] = x.into();
    x.into()
}

impl<DB: Database + Clone, P: TreeParams> Clone for MerkleTreeVersion<DB, P>
where
    P::Hasher: Clone,
{
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            block: self.block,
            read_storage_tree_dev_log_file: self.read_storage_tree_dev_log_file.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn read_storage_tree_dev_log_is_written() {
        let temp_file = NamedTempFile::new().unwrap();
        let log_file = temp_file.reopen().unwrap();
        let log_file = Some(Arc::new(Mutex::new(log_file)));

        let key = Bytes32Dev::from_array([0x11; 32]);
        let output = Some(7_u64);
        write_read_storage_tree_dev_log(&log_file, 42, "tree_index", &key, &output);

        let contents = std::fs::read_to_string(temp_file.path()).unwrap();
        assert!(contents.contains("block=42"));
        assert!(contents.contains("method=tree_index"));
        assert!(
            contents.contains(
                "input=0x1111111111111111111111111111111111111111111111111111111111111111"
            )
        );
        assert!(contents.contains("output=Some(7)"));
    }
}
