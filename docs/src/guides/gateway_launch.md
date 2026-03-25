# Gateway Launch (Syscoin + OS Server)

Gateway + optional **edge** chain with `zkstack` (`--zksync-os`), `zksync-os-server`, and Airbender. Target: Gateway `rollup` + `Blobs`; edge `rollup` + `RelayedL2Calldata`.

**Prerequisites:** `zksync-os-scripts/docs/src/prerequisites.md`. **Pinned SHAs:** `local-chains/<protocol_version>/versions.yaml`:
- top-level `zksync-era` / `zkstack_cli`: `zkstack-cli.sha`
- nested `zksync-era/contracts`: `era-contracts.sha`

---

## Run (one script)

All steps are driven by **`scripts/gateway-launch/run-gateway-launch.sh`**. Sub-scripts in the same directory are for advanced / piecemeal use only.

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
export ZKSYNC_OS_SERVER_PATH=/path/to/zksync-os-server   # optional; defaults to this repo

# Disposable local L1 (starts Anvil + watch in background, logs to fixed paths)
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 anvil

# Tanenbaum — local NEVM (same contract as Anvil: HTTP JSON-RPC on loopback)
export L1_RPC_URL=http://127.0.0.1:8545
export FUNDER_PRIVATE_KEY=0x…   # funded on Tanenbaum (NOT the Anvil default key)
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 tanenbaum

# Tanenbaum — remote HTTPS RPC only if you accept provider rate limits / ops constraints
# export L1_RPC_URL=https://…
# bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 tanenbaum

# Syscoin mainnet
export L1_RPC_URL=https://your-mainnet-rpc.example
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 mainnet
```

**`run-gateway-launch.sh --help`** lists flags (`--reuse-ecosystem`, `--reset-l1-artifacts`, `--skip-fund`, `--with-edge`, `--migrate-edge`, `--stop-after-l1`, `--no-start-anvil`, …).

**Default log:** `~/gateway-launch.log` (override **`GATEWAY_LAUNCH_LOG`** or **`--log PATH`**). **Anvil-only:** Anvil stdout → **`~/gateway-local-anvil.log`**, watch notes → **`~/gateway-local-anvil-watch.log`**.

### Edge chain: what runs by default

| Goal | Flags |
|------|--------|
| **Gateway only** (ecosystem + L1 deploy + gateway `chain init` + convert to gateway settlement) | none — this is the default |
| **You want an edge rollup in the ecosystem** | add **`--with-edge`** (create + init edge; see **`EDGE_CHAIN_NAME` / `EDGE_CHAIN_ID`** below) |
| **Edge should settle through the gateway** | add **`--migrate-edge`** after the edge chain exists **and** the **Gateway chain L2 JSON-RPC** is reachable. Keep the **edge node stopped** until migration/finalization completes (the script does not start `zksync-os-server`; see [After the script](#after-the-script)) |

**`--with-edge`** and **`--migrate-edge`** are independent: you can create the edge in one run (`--with-edge`) and run migration later (second run with **`--reuse-ecosystem`** + **`--migrate-edge`**, or only **`--migrate-edge`** if the edge is already in **`ZkStack.yaml`**). For a single shot when RPC is already up: `…/run-gateway-launch.sh --l1 anvil --with-edge --migrate-edge`.

---

## `--l1` profiles (settings the script applies)

| `--l1`     | `L1_CHAIN_ID` | `L1_NETWORK` (zkstack) | `L1_RPC_URL`        |
|------------|-----------------|-------------------------|---------------------|
| `anvil`    | 9               | `localhost`             | default `http://127.0.0.1:8545` |
| `tanenbaum` | 5700           | `tanenbaum`             | **HTTP(S) `L1_RPC_URL`** (e.g. local `http://127.0.0.1:8545` with `sysgeth --http`, or remote HTTPS) |
| `mainnet`  | 57              | `mainnet`               | **HTTP(S) `L1_RPC_URL`** |

**Do not** use **IPC / unix** URLs for `L1_RPC_URL` (e.g. `geth.ipc`): the launch path is built on Foundry **`cast` / `forge`** (`--rpc-url`), which require HTTP(S). Enable **`sysgeth --http`** for local Tanenbaum, same idea as Anvil’s **`http://127.0.0.1:8545`**.

**Do not** run Anvil with chain id **5700** and `--l1-network tanenbaum`: `zkstack` only enables `with_slow()` for **`localhost`**; large L1 deploys stall on loopback without it. Use **real** Tanenbaum (NEVM HTTP or remote RPC), or Anvil **9** + **`anvil`**.

**Local Anvil:** `anvil-local-start.sh` uses **`--mixed-mining`** with a long **`--block-time`** (default **`3600`**, override with **`GATEWAY_ANVIL_BLOCK_TIME`**). This is intentional: newer Anvil builds reject **`--mixed-mining`** without **`--block-time`**, while **`--block-time 1`** left **`forge script --broadcast`** stuck on **`DeployL1CoreContracts`** in practice: **`run-latest.json`** fills **`transactions`** but **`receipts` stay empty** while **`txpool_status`** shows **`pending = queued = 0`**. The long interval satisfies the newer CLI, and the background **watch** remains the effective miner by calling **`anvil_mine 1` every ~1s** during large deploys so mining still runs when the pool reads empty. **`--mixed-mining`** still helps the [foundry#10122](https://github.com/foundry-rs/foundry/issues/10122) class (concurrent / out-of-order nonces).

---

## Env you may set

| Variable | Purpose |
|----------|---------|
| `PROTOCOL_VERSION` | e.g. `v31.0`; pins `versions.yaml` for both `REQUIRED_ZKSTACK_CLI_SHA` and `REQUIRED_CONTRACTS_SHA` |
| `REQUIRED_ZKSTACK_CLI_SHA` | optional override; else read from `versions.yaml` (`zkstack-cli.sha`) |
| `REQUIRED_CONTRACTS_SHA` | optional override; else read from `versions.yaml` |
| `GATEWAY_DIR` | default `~/gateway` |
| `GATEWAY_ECOSYSTEM_PARENT_DIR` | parent dir for `ecosystem create` (default `$HOME`) |
| `FUNDER_PRIVATE_KEY` | funding txs on L1; **anvil** profile defaults to Anvil dev key 0; **tanenbaum** / **mainnet** you must set a key with native L1 balance |
| `L1_RPC_URL` | **Must** be `http://` or `https://` JSON-RPC for **tanenbaum** / **mainnet** (not IPC). Prefer **local** `sysgeth --http` to avoid public-RPC rate limits. |
| `EDGE_CHAIN_NAME` / `EDGE_CHAIN_ID` | edge chain (defaults `zksys` / `57057`) with `--with-edge` |
| `FOUNDRY_EVM_VERSION` | default `shanghai` for this contracts pin |

---

## One-time machine setup

1. Foundry + foundry-zksync (`prerequisites.md`).
2. Pin `zksync-era/contracts` to `era-contracts.sha` from `versions.yaml` if needed: **`preflight-pin-era-contracts.sh`** (creates a commit).
3. `bash scripts/gateway-launch/preflight-zkstack-cli.sh`
This verifies top-level `zksync-era` against `zkstack-cli.sha`, verifies `zksync-era/contracts` against `era-contracts.sha`, then applies the Syscoin patch and builds the repo-local `zkstack`.
4. If either object is missing locally, fetch it explicitly:
`git -C "${ZKSYNC_ERA_PATH}" fetch origin "${REQUIRED_ZKSTACK_CLI_SHA}"`
`git -C "${ZKSYNC_ERA_PATH}/contracts" fetch origin "${REQUIRED_CONTRACTS_SHA}"`

---

## After the script

- **Gateway / edge nodes:** `cargo run --release -- --config config-presets/testnet-gateway.yaml` (and `testnet-child.yaml` for the edge). Presets live under `zksync-os-server/config-presets/`.
- **Generated OS-server configs:** the launcher writes runnable layered configs under `"$GATEWAY_DIR/os-server-configs/"`.
Gateway: `"$GATEWAY_DIR/os-server-configs/gateway/start-node.sh"`
Edge: `"$GATEWAY_DIR/os-server-configs/$EDGE_CHAIN_NAME/start-node.sh"` (after `--with-edge`)
- **`--migrate-edge`:** requires the Gateway L2 RPC to be up **before** `zkstack chain gateway migrate-to-gateway` can succeed. The **edge node should remain stopped** until both `migrate-to-gateway` and `finalize-chain-migration-to-gateway` are done; start the edge only afterward with the generated gateway-linked config. If the node is remote, set `api.web3_json_rpc.http_url` on the gateway chain config. See [Edge chain flags](#edge-chain-what-runs-by-default) for when to pass **`--with-edge`** vs **`--migrate-edge`**.
- **Provers:** Airbender per chain; not Era-only `zkstack prover`.
- **`token_weth_address`** in `configs/initial_deployments.yaml` for non-local L1s as required by your ops.

---

## Stuck L1 deploy / recovery

**Disposable Anvil:** restart Anvil (fresh chain), then either full re-run with **`--reset-l1-artifacts`**, or **`gateway-l1-reset-local.sh`** after **`--reuse-ecosystem`** (see that script). **Remote L1:** operational recovery only (salt / new ecosystem / collision avoidance).

**Diagnose:** inspect **`txpool_status`** **`pending`** and **`queued`**. The watch mines on a fixed interval regardless, but if Forge still spins compare **`jq '.receipts|length'`** vs **`jq '.transactions|length'`** on `broadcast/.../run-latest.json`: **0 receipts** with **non-empty transactions** means Forge never finished the broadcast on-chain — kill **`forge`/`zkstack`**, fresh Anvil, **`--reset-l1-artifacts`**, re-run. Also check **phantom broadcast** (`cast tx` on last hash fails).

**SSH:** avoid `pkill -f` patterns that match your own `bash -c` line; use **`killall forge`** / **`killall zkstack`** or bracketed `pkill` patterns.

---

## Gotchas

- `deployer` / `governor` need **≥ 5.5** native on L1 before `zkstack` balance checks.
- `zk_token_asset_id must be non-zero` / `is_zk_sync_os`: handled inside **`gateway-deploy-l1.sh`**.
- `governanceAcceptOwnerAggregated` / selector mismatch: pin `zkstack` + contracts per **`versions.yaml`**.
