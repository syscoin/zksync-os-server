# Canonical V32 restore supervisor

The integration harness supplies an independently trusted descriptor digest, an
absolute canonical fixture directory, an empty owner-only run directory, and an
absolute owner-only private config directory. Keep the supervisor's stdin pipe
open for as long as the restored Core/NEVM pair is needed. Close it after
stopping the in-process Gateway and edge nodes. A zero exit means that the
supervisor also stopped the Core and NEVM processes cleanly.

The supervisor is the exact `restore_supervisor` tool described in
`l1-backend.json`. It verifies the descriptor before staging archives and starts
only its verified Core and Geth binaries. It refuses the regeneration marker.

## Producer records

- `snapshot_manifest` uses schema `syscoin-v32-canonical-snapshot-layout-v1`
  with a `roles` object. Its keys are the five snapshot roles in the descriptor.
  Each value contains the exact integer `files` and `extracted_bytes` of
  regular archive members. Use tar archives with normalized relative
  paths. Strip runtime locks, cookies, IPC sockets, and mutable settings before
  packing. The Core archive contains `regtest/` and the NEVM archive contains the
  Geth datadir contents, including `geth/`. Each rollup archive contains
  `rocksdb/` and may include `proof_storage/` or `replay_archive/`.
- `deployment_record` uses schema `syscoin-v32-canonical-private-configs-v1`.
  Its remaining fields are the private `core_wallet_name`, the
  `nevm_miner` address, and `roles` with keys `gateway`, `edge-6565` and
  `edge-6566`. Each role contains `config` and `genesis` file identities
  (path, size, SHA-256). Supply `<role>.json` and `<role>.genesis.json` in
  the private config directory. Make each file owner-only. The receipt binds
  their exact bytes. The supervisor stages them as `config.json` and
  `genesis.json` under the role directory. The harness configures per-run
  local endpoints and paths when constructing each node.
- `clean_boundary` uses schema `syscoin-v32-canonical-clean-boundary-v1`.
  Supply `core_height`, `core_hash`, `core_genesis_hash`,
  `core_nevm_hash`, `nevm_height`, `nevm_hash`, `nevm_genesis_hash`,
  and `roles` with the same three rollup names. Each rollup row contains
  `block_number`, `block_hash` and `batch_number`. Hashes use lowercase
  `0x` followed by 64 hex digits. Preserve the last committed Geth head even
  when Core has one further buffered block. The consumer verifies live rollup
  frontiers after booting its nodes.

## Readiness

`ready.json` is written atomically in the run directory once the daemons are
ready. Its schema is `syscoin-v32-canonical-restore-ready-v1`. It contains
`descriptor_sha256`, `core_rpc_url`, `nevm_rpc_url`, `core_cookie_file`
(relative to the run directory), `core_wallet_name`, `nevm_latest_timestamp`
(Unix seconds), and `roles` copied from the authenticated clean boundary.
Both RPC URLs are numeric IPv4 loopback with unique owned listeners. Keep the
run directory private. It contains the Core cookie, private configs, daemon
logs, and disposable state.
