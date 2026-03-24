# Gateway Launch (Syscoin + OS Server)

Gateway + optional **edge** chain with `zkstack` (`--zksync-os`), `zksync-os-server`, and Airbender. Target: Gateway `rollup` + `Blobs`; edge `rollup` + `RelayedL2Calldata`.

**Prerequisites:** `zksync-os-scripts/docs/src/prerequisites.md`. **Contracts SHA:** `local-chains/<protocol_version>/versions.yaml` (`era-contracts.sha`).

---

## Run (one script)

All steps are driven by **`scripts/gateway-launch/run-gateway-launch.sh`**. Sub-scripts in the same directory are for advanced / piecemeal use only.

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
export ZKSYNC_OS_SERVER_PATH=/path/to/zksync-os-server   # optional; defaults to this repo

# Disposable local L1 (starts Anvil + watch in background, logs to fixed paths)
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 anvil

# Tanenbaum — you must set RPC (and funders; see below)
export L1_RPC_URL=https://your-tanenbaum-rpc.example
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/gateway-launch/run-gateway-launch.sh" --l1 tanenbaum

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
| **Edge should settle through the gateway** | add **`--migrate-edge`** after the edge chain exists **and** the **Gateway chain L2 JSON-RPC** is reachable (the script does not start `zksync-os-server`; see [After the script](#after-the-script)) |

**`--with-edge`** and **`--migrate-edge`** are independent: you can create the edge in one run (`--with-edge`) and run migration later (second run with **`--reuse-ecosystem`** + **`--migrate-edge`**, or only **`--migrate-edge`** if the edge is already in **`ZkStack.yaml`**). For a single shot when RPC is already up: `…/run-gateway-launch.sh --l1 anvil --with-edge --migrate-edge`.

---

## `--l1` profiles (settings the script applies)

| `--l1`     | `L1_CHAIN_ID` | `L1_NETWORK` (zkstack) | `L1_RPC_URL`        |
|------------|-----------------|-------------------------|---------------------|
| `anvil`    | 9               | `localhost`             | default `http://127.0.0.1:8545` |
| `tanenbaum` | 5700           | `tanenbaum`             | **you must `export L1_RPC_URL`** |
| `mainnet`  | 57              | `mainnet`               | **you must `export L1_RPC_URL`** |

**Do not** run Anvil with chain id **5700** and `--l1-network tanenbaum`: `zkstack` only enables `with_slow()` for **`localhost`**; large L1 deploys stall on loopback without it. Use **real** Tanenbaum RPC, or Anvil **9** + **`anvil`**.

**Local Anvil:** `anvil-local-start.sh` uses **`--mixed-mining`** only (no **`--block-time`**). In practice, **`--block-time 1`** here left **`forge script --broadcast`** stuck on **`DeployL1CoreContracts`**: **`run-latest.json`** fills **`transactions`** but **`receipts` stay empty** while **`txpool_status`** shows **`pending = queued = 0`**. **`--mixed-mining`** still helps the [foundry#10122](https://github.com/foundry-rs/foundry/issues/10122) class (concurrent / out-of-order nonces). The background **watch** logs **`pending` / `queued`** when non-zero and calls **`anvil_mine 1` every ~1s** during large deploys so mining still runs when the pool reads empty.

---

## Env you may set

| Variable | Purpose |
|----------|---------|
| `PROTOCOL_VERSION` | e.g. `v31.0`; pins `versions.yaml` for `REQUIRED_CONTRACTS_SHA` |
| `REQUIRED_CONTRACTS_SHA` | optional override; else read from `versions.yaml` |
| `GATEWAY_DIR` | default `~/gateway` |
| `GATEWAY_ECOSYSTEM_PARENT_DIR` | parent dir for `ecosystem create` (default `$HOME`) |
| `FUNDER_PRIVATE_KEY` | funding txs on L1; **anvil** profile defaults to Anvil dev key 0 |
| `EDGE_CHAIN_NAME` / `EDGE_CHAIN_ID` | edge chain (defaults `zksys` / `57057`) with `--with-edge` |
| `FOUNDRY_EVM_VERSION` | default `shanghai` for this contracts pin |

---

## One-time machine setup

1. Foundry + foundry-zksync (`prerequisites.md`).
2. `bash scripts/gateway-launch/preflight-zkstack-cli.sh`
3. Pin `zksync-era/contracts` to the SHA in `versions.yaml` if needed: **`preflight-pin-era-contracts.sh`** (creates a commit).
4. `git -C "${ZKSYNC_ERA_PATH}/contracts" fetch origin "${REQUIRED_CONTRACTS_SHA}"` if the object is missing.

---

## After the script

- **Gateway / edge nodes:** `cargo run --release -- --config config-presets/testnet-gateway.yaml` (and `testnet-child.yaml` for the edge). Presets live under `zksync-os-server/config-presets/`.
- **`--migrate-edge`:** requires this Gateway L2 RPC to be up **before** `zkstack chain gateway migrate-to-gateway` can succeed; if the node is remote, set `api.web3_json_rpc.http_url` on the gateway chain config. See [Edge chain flags](#edge-chain-what-runs-by-default) for when to pass **`--with-edge`** vs **`--migrate-edge`**.
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
