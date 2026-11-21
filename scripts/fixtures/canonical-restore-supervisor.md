# Canonical V32 restore supervisor

The integration harness supplies an independently trusted descriptor digest, an
absolute canonical fixture directory, an empty owner-only run directory, and an
absolute owner-only private config directory. Keep the supervisor's stdin pipe
open for as long as the restored Core/NEVM pair is needed. Send `mine\n` once
after authenticating readiness to start paired mining. Close the pipe after
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

After the mining command, each new Core block must advance the native NEVM
commitment, and SysGeth must expose the immediately preceding commitment at
its pinned height. This allows the native one-block buffer while ensuring the
restored settlement chain continues advancing.

## Canonical release qualification

Run the private lanes from the exact reviewed commit on the owned test
infrastructure. Supply the fixture and configuration through
`SYSCOIN_V32_CANONICAL_FIXTURE_DIR` and `SYSCOIN_CANONICAL_PRIVATE_CONFIG_DIR`.
Check the complete fixture with
`python3 -B scripts/apply-era-contracts-syscoin-release.py --check-canonical-fixture`.
Run the CPU restore smoke before the CUDA proving lane.

```sh
bash scripts/cargo-with-patched-zksync-os.sh canonical-restore-smoke -- \
  nextest run --locked --release -p zksync_os_integration_tests \
  --features prover-tests -E 'binary(prover) & test(~canonical_restore_smoke)' \
  --no-tests fail

python3 -B scripts/apply-era-contracts-syscoin-release.py --check-canonical-prover-assets
bash scripts/cargo-with-patched-zksync-os.sh canonical-settlement-cuda -- \
  nextest run --locked --release -p zksync_os_integration_tests \
  --features gpu-prover-tests -E 'binary(prover) & not test(~canonical_restore_smoke)' \
  --no-tests fail
```

The CUDA lane uses the pinned worker binary, guest, and compact CRS supplied
through `SYSCOIN_V8_PROVER_BIN`, `SYSCOIN_V8_APP_BIN`, and `COMPACT_CRS_FILE`.
Retain the private run evidence for both lanes. After each lane passes, the
approved operator can attest its success on the exact tested commit in
`syscoin/zksync-os-server`. Use the commit status context
`syscoin/v32/canonical-restore-cpu` or `syscoin/v32/canonical-settlement-cuda`
and the description `v1 tree=<git tree SHA> descriptor=<descriptor SHA-256>`.
The digest must equal the descriptor verified by the private tests and
registered by `--check-canonical-fixture-binding`. Publish only these public
identities. Use the operator's separate, repository-scoped status credential
outside Actions.

`SYSCOIN_CANONICAL_QUALIFIER_ID` is the approved operator's numeric GitHub
account ID in the repository's Actions variables. Run the manual **Canonical
release qualification** workflow with `source_sha` set to the exact tested
release commit. The workflow reads the latest status from that account for
both lanes with its own read-only token. Both must match that commit, its Git
tree, and the registered private descriptor.

The binary publisher, the binary recovery workflow, and versioned Docker
publishing require that same exact qualification. A tag must peel to the
qualified commit. Retrying the same tag and commit reuses the original CPU
and CUDA results while the latest trusted statuses still succeed. A new merge
commit, version bump, squash, or rebase needs qualification at the resulting
commit. Creating a release tag or publishing release-note metadata does not
certify its assets. The private fixture remains on the owned test host.

Ordinary PRs, main pushes, and stress CI keep their normal builds and tests,
including the prover integration test compilation. They need no private proof
status. An ordinary main push also retains its existing development image
tags. A pending fixture blocks versioned publication while normal CI remains
available.
