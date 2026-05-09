# Gateway Launch (Checkpointed Canonical Flow)

Gateway + edge launch is now a **single canonical command** with checkpointed resume and explicit repair.

## Canonical command

Start local Syscoin RPC bridge first (Tanenbaum/Mainnet launcher expects local `L1_RPC_URL`):

```bash
./syscoind --testnet -server=1 -daemon=1 \
  -gethcommandline=--http \
  -gethcommandline=--http.addr=127.0.0.1 \
  -gethcommandline=--http.port=8545 \
  -gethcommandline=--http.api=eth,net,web3 \
  -gethcommandline=--http.vhosts=localhost,127.0.0.1
```

Keep the local RPC bound to loopback and do not enable wildcard CORS. Only add
extra namespaces such as `txpool` or `debug` for a short, trusted debugging
session, then restart the node with the restricted API list above.

Run from a `zksync-os-server` clone:

```bash
cd /path/to/zksync-os-server
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.tanenbaum.io
export FUNDER_PRIVATE_KEY=0x...
export EDGE_GATEWAY_GOVERNOR_SIGNER=account
export EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME=governor
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --migrate-edge
```

Mainnet:

```bash
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.syscoin.org
export FUNDER_PRIVATE_KEY=0x...
export EDGE_GATEWAY_GOVERNOR_SIGNER=account
export EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME=governor
bash scripts/gateway-launch/run-gateway-launch.sh --l1 mainnet --migrate-edge
```

The governor signer is used only for Gateway migration repair transactions. Do not pass this key as a raw command-line argument. Import it into Foundry's keystore first with `cast wallet import governor --interactive`, or use `EDGE_GATEWAY_GOVERNOR_SIGNER=keystore`, `ledger`, `trezor`, `aws`, or `gcp`.
The `--migrate-edge` flag is required for the normal full launch because it pauses deposits and finalizes the edge-chain migration to Gateway settlement. If omitted, the launcher stops after edge-chain initialization and can be resumed later with the same command plus `--migrate-edge`.

Optional log override:

```bash
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --log /tmp/gateway-launch.log
```

To intentionally continue from an existing ecosystem directory that already has
`ZkStack.yaml`, pass `--reuse-ecosystem`. Without this flag, the launcher fails
instead of silently bypassing wallet creation controls.

## Checkpoint model

State file:

```text
$GATEWAY_DIR/.gateway-launch/state.json
```

Ordered checkpoints:

1. `gl.workspace`
2. `gl.ecosystem`
3. `gl.wallets_funded`
4. `gl.l1_ecosystem_deployed`
5. `gl.gateway_chain_inited`
6. `gl.gateway_settlement`
7. `gl.os_configs_gateway`
8. `gl.edge_chain_inited`
9. `gl.migration`
10. `gl.os_configs_final`

Behavior:

- A checkpoint is skipped only when state says `passed` **and** probe validation passes.
- Any failure marks checkpoint `blocked` and stops the run.
- Resume is automatic on the same command after repair.
- Resume is rejected if fingerprinted launch context changed (network/chain/sha/env mismatch).

## Repair workflow

Inspect state:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum status
```

Repair one blocked checkpoint:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum repair gl.edge_chain_inited
```

Then rerun the canonical launcher command.

## What the launcher does

- Creates a new ecosystem, or reuses one only when `--reuse-ecosystem` is explicit.
- Funds required wallets.
- Deploys/initializes gateway contracts and chain.
- Converts gateway settlement.
- Creates/initializes edge chain.
- Runs edge migration to gateway settlement.
- Ensures DA pair correctness and deposit unpause via migration script guards.
- Generates final `os-server-configs` launchers.

## Start nodes after successful launch

```bash
"$GATEWAY_DIR/os-server-configs/gateway/start-node.sh"
"$GATEWAY_DIR/os-server-configs/zksys/start-node.sh"
```

The generated `start-node.sh` now preflights the open-file limit before starting the node:

- it tries to raise `ulimit -n` to `1048576`
- it warns if the resulting limit is below the recommended `131072`
- it fails fast if the resulting limit is below `65536`

If the script cannot raise the limit high enough, increase the shell / service hard limit first (for example via systemd `LimitNOFILE`, Docker `--ulimit`, or host user limits).

## Important env vars

| Variable | Purpose |
|---|---|
| `L1_RPC_URL` | Required HTTP(S) JSON-RPC endpoint used for broadcasts (expected: local Syscoin node/proxy, e.g. `http://127.0.0.1:8545`) |
| `GATEWAY_DIR` | Ecosystem workspace path (default `~/gateway`) |
| `REUSE_ECOSYSTEM` | Set to `true` only when intentionally reusing an existing `$GATEWAY_DIR/ZkStack.yaml`; equivalent to `--reuse-ecosystem` |
| `MIGRATE_EDGE` | Set to `true` only when intentionally pausing deposits and migrating/finalizing the edge chain; equivalent to `--migrate-edge` |
| `GATEWAY_ARCHIVE_L1_RPC_URL` | Recommended runtime archive RPC URL for gateway node + migration startup (if unset, falls back to `L1_RPC_URL`) |
| `FUNDER_PRIVATE_KEY` | Required when wallets need top-ups |
| `GATEWAY_FUND_WALLETS_PATHS` | Optional extra `wallets.yaml` paths to fund (colon-separated) |
| `PROVER_MODE` | `gpu` (default) or `no-proofs` |
| `PROTOCOL_VERSION` | Default `v31.0` |
| `GATEWAY_REUSE_ZKSYS_TOKEN` | Set to `true` only for explicit recovery reuse of an already deployed ZKSYS token; normal launches deploy ZKSYS and derive the asset id |
| `ZKSYNC_ERA_PATH` | Optional custom era checkout; otherwise launcher manages pinned workspace |
| `ZKSYNC_OS_DEV_PATH` | Optional custom upstream `zksync-os` checkout to patch for the `v31` dev proving line; otherwise launcher manages it under `$GATEWAY_DIR/.gateway-launch/zksync-os/` |
| `ZKSYNC_OS_GIT_URL` | Optional override for the upstream `zksync-os` Git URL used when launcher materializes the patched `dev` workspace |
| `GATEWAY_CREATE2_FACTORY_SALT` | Optional deterministic `create2_factory_salt` override for L1 deployment |
| `GATEWAY_WALLET_PATH` | Wallet file used for gateway ecosystem create (`in-file` if present, else random+persist) |
| `EDGE_WALLET_PATH` | Wallet file used for edge chain create (`in-file` if present, else random+persist) |
| `EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR` | Optional override for DA validator used by edge migration repair on Gateway |
| `EDGE_GATEWAY_GOVERNOR_SIGNER` | Governor signer backend for Gateway migration repairs: `account` (default), `keystore`, `ledger`, `trezor`, `aws`, or `gcp` |
| `EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME` | Foundry keystore account name when `EDGE_GATEWAY_GOVERNOR_SIGNER=account` (default `governor`) |
| `EDGE_GATEWAY_GOVERNOR_KEYSTORE` | Keystore file path when `EDGE_GATEWAY_GOVERNOR_SIGNER=keystore` |
| `EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE` | Optional keystore password file passed to Forge without exposing the password in argv |
| `BITCOIN_DA_RPC_URL` / `BITCOIN_DA_RPC_USER` / `BITCOIN_DA_RPC_PASSWORD` | DA connectivity for gateway blobs mode |

## Notes

- Default `FOUNDRY_EVM_VERSION` remains `shanghai`.
- `run-gateway-launch.sh` still enforces L1 chain-id preflight before broadcast steps.
- Migration safety guards remain in `edge-chain-migrate-to-gateway.sh` (DA bytecode checks, idempotent pause/unpause behavior).
- For Tanenbaum/Mainnet launches, keep `L1_RPC_URL` on local Syscoin RPC and set `GATEWAY_ARCHIVE_L1_RPC_URL` to the archive/public endpoint.
- Changing `GATEWAY_CREATE2_FACTORY_SALT` resets checkpoint state automatically (new redeploy run context).
- If you switch prover mode (`PROVER_MODE` / effective `GATEWAY_PROVER_MODE`) between runs, clear checkpoint state first: `rm -rf $GATEWAY_DIR/.gateway-launch`.
- During `gl.l1_ecosystem_deployed`, launcher clears `os-server-configs/gateway/db` before redeploy to avoid stale replay assertion panics.
- During `gl.edge_chain_inited`, launcher clears `os-server-configs/zksys/db` (or configured edge chain name) before re-init for the same reason.
- For `v31.x`, `start-node.sh` runs through a launcher wrapper that copies the current `zksync-os-server` tree into `$GATEWAY_DIR/.gateway-launch/zksync-os-server/`, rewrites only the `*_dev` `zksync-os` deps to the patched upstream checkout, and uses that isolated workspace for `cargo run`.
- High-TPS runs can exhaust low default `nofile` limits; use at least `65536`, with `131072+` recommended.
