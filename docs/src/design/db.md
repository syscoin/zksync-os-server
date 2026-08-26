# Database Schema Overview

Primary persistent node state is split across these RocksDB databases and proof files:

- block_replay_wal
- preimages_full_diffs
- repository
- state_full_diffs
- tree
- batch (`ExecutedBatchStorage`, RocksDB label `executed_batch_storage`)
- raft
- priority_txs_tree
<!-- SYSCOIN: Compact-DA HA producers retain the authentication epoch without running a batcher. -->
- batch_work_queue (batchers and compact-DA-capable HA producers); bitcoin_da_status (batchers)
- proofs (JSON files, not RocksDB)

<!-- SYSCOIN: These stores form one protocol/genesis-bound recovery set. -->

Back up or move the complete node database directory while the node is stopped;
do not restore, clear, or copy one store independently unless a documented
recovery procedure explicitly permits it. In particular, pre-reset V31 state is
not valid V32 input, even when the chain ID is unchanged.

<!-- SYSCOIN: Authenticate the complete storage root before any child database is opened. -->
The root contains a `database_identity.json` marker that binds protocol version, both chain IDs,
L1 genesis, the diamond proxy, and L2 genesis. The regular JSON marker is bounded to 16 KiB, created
with no-overwrite semantics, and synced before any child RocksDB is opened. A missing marker is
accepted only for a fresh otherwise-empty root. An identity mismatch, malformed or interrupted
marker, or unmarked legacy store fails startup; move or reset the entire directory rather than
replacing an individual marker or RocksDB. Host/operator filesystem mutation is outside this
deployment-mismatch guard's trust boundary.

---

## 1. block_replay_wal

Write-ahead log containing recent block data.

| Column | Key | Value |
|--------|-----|--------|
| block_output_hash | block number | Block output hash |
| context | block number | Binary-encoded BlockContext (BlockMetadataFromOracle) |
| last_processed_l1_tx_id | block number | ID (u64) of the last processed L1 tx in the block |
| txs | block number | Vector of EIP-2718 encoded transactions |
| node_version | block number | Node version that produced the block |
| latest | 'latest_block' | Latest block number |

---

## 2. preimages_full_diffs

| Column | Key | Value |
|--------|-----|--------|
| storage | hash | Preimage for the hash |

---

## 3. repository

Canonical blocks and transactions.

| Column | Key | Value |
|--------|-----|--------|
| initiator_and_nonce_to_hash | address (20 bytes) + nonce (u64) | Transaction hash |
| tx_meta | transaction hash | Binary TxMeta (hash, number, gas used, etc.) |
| block_data | block hash | Alloy-serialized block |
| tx_receipt | transaction hash | Binary EIP-2718 receipt |
| meta | 'block_number' | Latest block number |
| tx | transaction hash | EIP-2718 encoded bytes |
| block_number_to_hash | block number | Block hash |

---

## 4. state_full_diffs

Every storage write, keyed by hashed key and block number: a read at block B is a single
reverse seek from `hashed_key || B`.

| Column | Key | Value |
|--------|-----|--------|
| data | hashed_key (32 bytes) \|\| block number (8 bytes, big-endian) | Storage value |
| meta | 'latest_block' | Latest block number |

---

## 5. tree

Merkle-like structure.

| Column | Key | Value |
|--------|-----|--------|
| default | composite (version + nibble + index) | Serialized Leaf or Internal node |
| key_indices | hash | Key index |

Note: The 'default' column also stores a serialized Manifest at key '0'.

---

## 6. batch / executed_batch_storage

The node opens this RocksDB at `<rocks_db_path>/batch`; its internal metrics /
schema label is `executed_batch_storage`.

| Column | Key | Value |
|--------|-----|-------|
| BatchInfo | batch number (u64, big-endian) | JSON `PersistedBatch` |
| FirstBlockIndex | first block number (u64, big-endian) | batch number (u64, big-endian) |
| Latest | `latest_batch` | highest appended batch number |

<!-- SYSCOIN: Document the bounded startup checks without reviving a full-store recovery walk. -->
This store anchors committed-batch discovery, stable batch RPC responses, and
restart recovery. Preserve it with the repository, replay WAL, state, and tree;
a malformed latest cursor, or a nonzero cursor without the canonical genesis,
fails startup rather than being repaired by deleting only this directory.

---

## 7. raft

When consensus is enabled, this database persists Raft logs, votes, committed
log metadata, membership/state-machine metadata, and the applied-WAL anchor.
Retain it with the replay WAL. Clearing it is a coordinated consensus operation,
not a general database reset.

---

## 8. priority_txs_tree

Stores the cached priority-operation Merkle tree and its last processed block.
It must remain aligned with executed/replayed block state.

---

## 9. proofs

Stored as JSON files in a separate directory:
../shared/fri_batch_envelopes
