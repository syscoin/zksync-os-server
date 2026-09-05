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

<!-- SYSCOIN: Preserve the independent inputs needed to authenticate recovery. -->
`syscoin_replay_identity_v1` additionally stores a domain-separated Keccak-256 identity of
each original replay input, excluding diagnostic `node_version`. It is written atomically with
the record and canonical header hash. The original full `context` remains the ancestry witness;
the compact context reconstructed from current canonical hashes is not a completion proof.
An ordinary duplicate write succeeds idempotently only if both the executed header and immutable
input identity match. It may repair derived state after a WAL-before-state crash, but cannot
replace the WAL. Only explicit rebuilds and external-node replacement retain overwrite permission.

Pre-identity experimental WALs cannot be authenticated by reconstructing an identity from mutable
indexes. They fail closed on duplicate replay / Raft recovery. There is no automatic migration:
retain the complete old recovery set and archives, and recover into a fresh, deployment-bound
directory through the supported archive workflow. Do not delete individual column families or
copy identity markers to make an old directory appear compatible.

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

<!-- SYSCOIN: Height reuse requires an immutable log-order acknowledgement journal. -->
The versioned applied journal retains `(LogId, block number, replay identity)` for every forwarded
normal entry, plus the WAL identity preceding the first journal entry at each height. Startup
finds the greatest log prefix whose complete set of touched heights matches the durable WAL;
matching one high block, or the current WAL height, is insufficient. Pending committed records
are replayed before leader proposals, including across the asynchronous canonizer bridge.
Startup scans the retained journal (linear in journal length); it is not pruned automatically.

Legacy height-only `RaftApplied` metadata and missing/malformed journal version metadata are
explicitly unsupported. There is no silent migration or standalone Raft reset. Preserve the
complete database set and use a coordinated recovery procedure before changing formats.

For an interrupted configured rebuild, the command source can replay an authenticated durable
prefix before the committed suffix. It requires the existing `from_block_hash` anchor to have
changed, or skipped transformations to be no-ops. An unchanged boundary with timestamp reset is
ambiguous and stops instead of guessing. Preserve the original rebuild configuration across
restarts; changing its transformation parameters mid-operation is not supported.

---

## 8. priority_txs_tree

Stores the cached priority-operation Merkle tree and its last processed block.
It must remain aligned with executed/replayed block state.

---

## 9. proofs

Stored as JSON files in a separate directory:
../shared/fri_batch_envelopes
