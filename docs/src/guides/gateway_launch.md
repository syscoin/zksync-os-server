# Gateway Launch (Checkpointed Canonical Flow)

Gateway + edge launch is now a **single canonical command** with checkpointed resume and explicit repair.

## Canonical command

Start local Syscoin RPC bridge first (Tanenbaum/Mainnet launcher expects local `L1_RPC_URL`):

```bash
./syscoind --testnet -server=1 -daemon=1 \
  -gethcommandline=--http \
  -gethcommandline=--http.addr=127.0.0.1 \
  -gethcommandline=--http.port=8545 \
  -gethcommandline=--http.api=eth,net,web3,txpool,debug \
  -gethcommandline=--http.vhosts=* \
  -gethcommandline=--http.corsdomain=*
```

Run from a `zksync-os-server` clone:

```bash
cd /path/to/zksync-os-server
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.tanenbaum.io
export FUNDER_PRIVATE_KEY=0x...
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum
```

Mainnet:

```bash
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.syscoin.org
export FUNDER_PRIVATE_KEY=0x...
bash scripts/gateway-launch/run-gateway-launch.sh --l1 mainnet
```

Optional log override:

```bash
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --log /tmp/gateway-launch.log
```

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

## What the launcher now always does

- Creates/reuses ecosystem.
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

## Important env vars

| Variable | Purpose |
|---|---|
| `L1_RPC_URL` | Required HTTP(S) JSON-RPC endpoint used for broadcasts (expected: local Syscoin node/proxy, e.g. `http://127.0.0.1:8545`) |
| `GATEWAY_DIR` | Ecosystem workspace path (default `~/gateway`) |
| `GATEWAY_ARCHIVE_L1_RPC_URL` | Recommended runtime archive RPC URL for gateway node + migration startup (if unset, falls back to `L1_RPC_URL`) |
| `FUNDER_PRIVATE_KEY` | Required when wallets need top-ups |
| `GATEWAY_FUND_WALLETS_PATHS` | Optional extra `wallets.yaml` paths to fund (colon-separated) |
| `PROVER_MODE` | `gpu` (default) or `no-proofs` |
| `PROTOCOL_VERSION` | Default `v31.0` |
| `ZKSYNC_ERA_PATH` | Optional custom era checkout; otherwise launcher manages pinned workspace |
| `GATEWAY_CREATE2_FACTORY_SALT` | Optional deterministic `create2_factory_salt` override for L1 deployment |
| `GATEWAY_WALLET_PATH` | Wallet file used for gateway ecosystem create (`in-file` if present, else random+persist) |
| `EDGE_WALLET_PATH` | Wallet file used for edge chain create (`in-file` if present, else random+persist) |
| `EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR` | Optional override for DA validator used by edge migration repair on Gateway |
| `BITCOIN_DA_RPC_URL` / `BITCOIN_DA_RPC_USER` / `BITCOIN_DA_RPC_PASSWORD` | DA connectivity for gateway blobs mode |

## Notes

- Default `FOUNDRY_EVM_VERSION` remains `shanghai`.
- `run-gateway-launch.sh` still enforces L1 chain-id preflight before broadcast steps.
- Migration safety guards remain in `edge-chain-migrate-to-gateway.sh` (DA bytecode checks, idempotent pause/unpause behavior).
- For Tanenbaum/Mainnet launches, keep `L1_RPC_URL` on local Syscoin RPC and set `GATEWAY_ARCHIVE_L1_RPC_URL` to the archive/public endpoint.
- Changing `GATEWAY_CREATE2_FACTORY_SALT` resets checkpoint state automatically (new redeploy run context).
