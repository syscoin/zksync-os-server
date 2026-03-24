# Gateway Launch (Syscoin + OS Server)

This is the canonical guide for launching the Gateway topology with:

- upstream `zksync-era` control plane (`zkstack`)
- OS-chain mode (`--zksync-os`)
- `zksync-os-server` runtime
- Airbender proving

The goal topology is:

- Gateway chain: `rollup` pricing + `Blobs` runtime pubdata mode
- Child chain: `rollup` pricing + `RelayedL2Calldata` runtime pubdata mode

## Repos and prerequisites

- `zksync-era` (upstream; linked to your contracts fork)
- `era-contracts`
- `zksync-os-server`
- `zksync-airbender` and `zksync-airbender-prover`
- Syscoin L1 RPC and Bitcoin DA/PoDA connectivity

Use `zksync-os-scripts` prerequisites as the single source of truth for tool versions:

- `zksync-os-scripts/docs/src/prerequisites.md`

For this run, target `v30.2` prerequisites from that file (including pinned Foundry-zksync).
Do not use unpinned `foundryup-zksync` defaults for `v30.2`.

Pick the toolchain version by reading:

- `zksync-os-server/local-chains/<protocol_version>/versions.yaml`
- then install matching tool versions from `zksync-os-scripts/docs/src/prerequisites.md`

After installing prerequisites, verify versions before continuing:

```bash
source "$HOME/.cargo/env"
uv --version
node --version
yarn --version
cargo --version
forge --version
cast --version
anvil --version || true
anvil-zksync --version || true
```

Resolve required `era-contracts` SHA from
`zksync-os-server/local-chains/<protocol_version>/versions.yaml`
(SHA is the source of truth; do not rely on tags).

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
export ZKSYNC_OS_SERVER_PATH=/path/to/zksync-os-server
export PROTOCOL_VERSION=v30.2
export VERSIONS_YAML="${ZKSYNC_OS_SERVER_PATH}/local-chains/${PROTOCOL_VERSION}/versions.yaml"
export REQUIRED_CONTRACTS_SHA="$(python3 - <<'PY'
import os, re
text = open(os.environ["VERSIONS_YAML"], "r", encoding="utf-8").read()
m = re.search(r"era-contracts:\s*(?:\n\s*#.*)*\n\s*sha:\s*\"([0-9a-f]{40})\"", text)
if not m:
    raise SystemExit("era-contracts sha not found in versions.yaml")
print(m.group(1))
PY
)"

```

Build `zkstack` from your `zksync-era` checkout:

```bash
# Apply Syscoin/Tanenbaum compatibility patch to upstream era first.
bash /path/to/zksync-os-server/scripts/apply-zksync-era-syscoin-patch.sh /path/to/zksync-era

# Verify patch effects using grep (works on minimal servers).
grep -n "Tanenbaum" /path/to/zksync-era/core/lib/basic_types/src/network.rs
grep -n "Tanenbaum" /path/to/zksync-era/zkstack_cli/crates/types/src/l1_network.rs
grep -n "Mainnet => 57" /path/to/zksync-era/zkstack_cli/crates/types/src/l1_network.rs

curl -L https://raw.githubusercontent.com/matter-labs/zksync-era/main/zkstack_cli/zkstackup/install | bash
zkstackup --local
```

Run a hard preflight compile before any `ecosystem init` / `chain init`:

```bash
export FOUNDRY_EVM_VERSION=shanghai
cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
forge build --skip test
```

If this fails with opcode errors such as `tload` / `tstore` / `mcopy`, stop and fix
the pinned contracts SHA in `local-chains/<protocol_version>/versions.yaml` first.
Do not continue with deployment in a partially compatible state.

Generate `genesis.json` from the currently checked-out contracts before `ecosystem init`.
Do not copy a prebuilt file.

```bash
# REQUIRED_CONTRACTS_SHA is sourced from local-chains/<protocol_version>/versions.yaml above.
ACTUAL_CONTRACTS_SHA="$(git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD)"
test "${ACTUAL_CONTRACTS_SHA}" = "${REQUIRED_CONTRACTS_SHA}" || {
  echo "ERROR: contracts SHA mismatch. expected=${REQUIRED_CONTRACTS_SHA} actual=${ACTUAL_CONTRACTS_SHA}"
  exit 1
}

mkdir -p "${ZKSYNC_ERA_PATH}/etc/env/file_based"

# Generate genesis from contract artifacts.
if [ ! -d "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen" ]; then
  echo "ERROR: missing genesis generator at contracts/tools/zksync-os-genesis-gen"
  exit 1
fi

# Ensure modern cargo/rustup toolchain is used (avoid old system cargo).
export PATH="$HOME/.cargo/bin:$PATH"

cd "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen"
cargo run --release -- \
  --output-file "${ZKSYNC_ERA_PATH}/etc/env/file_based/genesis.json"
```

## Environment

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
export SYSCOIN_L1_CHAIN_ID=5700   # Tanenbaum; use 57 on mainnet
export L1_RPC_URL=http://localhost:8545

export FOUNDRY_EVM_VERSION=shanghai
export FOUNDRY_CHAIN_ID=${SYSCOIN_L1_CHAIN_ID}
```

## Funding before `ecosystem init`

Use explicit per-role funding before running any `init` command.
This avoids non-interactive prompt failures when balances are too low.

Operational baseline for this guide (Tanenbaum):

- `deployer`: keep at least `5.5` tSYS at all times (`zkstack` hard-check is `>= 5`)
- `governor`: fund at least `5.5` tSYS
- `operator`, `blob_operator`, `prove_operator`, `execute_operator`, `fee_account`, `token_multiplier_setter`: fund `1` tSYS each

`--prover-mode gpu` is the source of truth for non-mock proving in this flow.
`zkstack` derives `testnet_verifier` from prover mode during deploy-input generation
(`gpu => false`, `no-proofs => true`).

## 1) Create Gateway ecosystem

```bash
export GATEWAY_CHAIN_ID=57001
export GATEWAY_PROVER_MODE=gpu
export GATEWAY_COMMIT_MODE=rollup

zkstack ecosystem create \
  --ecosystem-name gateway \
  --l1-network tanenbaum \
  --link-to-code ${ZKSYNC_ERA_PATH} \
  --chain-name gateway \
  --chain-id ${GATEWAY_CHAIN_ID} \
  --prover-mode ${GATEWAY_PROVER_MODE} \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode ${GATEWAY_COMMIT_MODE} \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default true \
  --evm-emulator false \
  --start-containers false \
  --zksync-os
```

## 2) Deploy ecosystem contracts

```bash
cd gateway
export GATEWAY_DIR="$(pwd)"
# set weth address by updating yaml files before you deploy
# Tanenbaum WETH
# configs/initial_deployments.yaml -> token_weth_address: 0xa66b2E50c2b805F31712beA422D0D9e7D0Fd0F35
# Mainnet WETH
# token_weth_address: 0xd3e822f3ef011Ca5f17D82C956D952D8d7C3A1BB
#
export FOUNDRY_EVM_VERSION=shanghai
export FOUNDRY_CHAIN_ID=${SYSCOIN_L1_CHAIN_ID}

# After `ecosystem create`, pin contracts to required SHA and apply Syscoin patch.
cd "${ZKSYNC_ERA_PATH}"
git submodule update --init contracts
cd contracts
git fetch origin "${REQUIRED_CONTRACTS_SHA}"
git cat-file -e "${REQUIRED_CONTRACTS_SHA}^{commit}" || {
  echo "ERROR: required era-contracts SHA not available: ${REQUIRED_CONTRACTS_SHA}"
  exit 1
}
git checkout "${REQUIRED_CONTRACTS_SHA}"

# Re-sync submodule URLs from .gitmodules and re-initialize recursively AFTER
# checkout so nested refs/URLs match the pinned commit state.
git submodule sync --recursive
git submodule update --init --recursive

# Ensure nested zksync-contracts submodule matches the exact SHA referenced by
# the checked-out era-contracts commit.
EXPECTED_NESTED_SHA="$(git ls-tree HEAD lib/@matterlabs/zksync-contracts | awk '{print $3}')"
test "$(git -C lib/@matterlabs/zksync-contracts rev-parse HEAD)" = "${EXPECTED_NESTED_SHA}" || {
  echo "ERROR: nested zksync-contracts SHA mismatch"
  exit 1
}

bash /path/to/zksync-os-server/scripts/apply-era-contracts-syscoin-patch.sh "${ZKSYNC_ERA_PATH}/contracts"

# Preflight compile and genesis generation from the pinned+patched contracts.
cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
forge build --skip test
cd "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen"
export PATH="$HOME/.cargo/bin:$PATH"
cargo run --release -- \
  --output-file "${ZKSYNC_ERA_PATH}/etc/env/file_based/genesis.json"

zkstack dev contracts

# Deploy the Gateway ZK token as ZKSYS and derive zk_token_asset_id for CTM.
# Do not hardcode Mainnet's asset id on Tanenbaum.
cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"

# DeployErc20 reads create2 settings from PERMANENT_VALUES_INPUT.
# Materialize it from the ecosystem's generated initial deployments config.
export PERMANENT_VALUES_INPUT="/script-config/permanent-values.toml"
export TOKENS_CONFIG="/script-config/config-deploy-erc20.toml"

CREATE2_FACTORY_SALT="$(python3 - <<'PY'
import yaml
from pathlib import Path
import os
p = Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml"
d = yaml.safe_load(p.read_text(encoding="utf-8"))
print(d["create2_factory_salt"])
PY
)"

# Prefer create2 factory from generated config if present; otherwise use
# the deterministic factory address used by contracts tooling.
CREATE2_FACTORY_ADDR="$(python3 - <<'PY'
import yaml
from pathlib import Path
import os
p = Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml"
d = yaml.safe_load(p.read_text(encoding="utf-8"))
print(d.get("create2_factory_addr", "0x4e59b44847b379578588920cA78FbF26c0B4956C"))
PY
)"

# Ensure the selected create2 factory is deployed on the target L1.
if [ "$(cast code "${CREATE2_FACTORY_ADDR}" --rpc-url "${L1_RPC_URL}")" = "0x" ]; then
  echo "ERROR: create2 factory has no code at ${CREATE2_FACTORY_ADDR}"
  exit 1
fi

cat > script-config/permanent-values.toml <<EOF
[permanent_contracts]
create2_factory_salt = "${CREATE2_FACTORY_SALT}"
create2_factory_addr = "${CREATE2_FACTORY_ADDR}"
EOF

cat > script-config/config-deploy-erc20.toml <<'EOF'
additional_addresses_for_minting = []

[tokens.ZKSYS]
name = "ZKSYS"
symbol = "ZKSYS"
decimals = 18
implementation = "TestnetERC20Token.sol"
# Keep within TOML integer parser limits used by forge parsing.
mint = 1000000000000000000
EOF

# Use the ecosystem deployer key.
# Replace this with your actual deployer private key for gateway/configs/wallets.yaml.
export DEPLOYER_PRIVATE_KEY="REPLACE_WITH_DEPLOYER_PRIVATE_KEY_HEX_WITHOUT_0X"

forge script deploy-scripts/tokens/DeployErc20.s.sol \
  --legacy \
  --ffi \
  --rpc-url "${L1_RPC_URL}" \
  --private-key "${DEPLOYER_PRIVATE_KEY}" \
  --broadcast

export ZKSYS_L1_TOKEN_ADDRESS="$(python3 - <<'PY'
import re
from pathlib import Path
text = Path("script-out/output-deploy-erc20.toml").read_text(encoding="utf-8")
# Parse address inside [tokens.ZKSYS] block.
block = re.search(r'(?ms)^\[tokens\.ZKSYS\]\s*(.*?)^\[', text + "\n[", re.MULTILINE)
if not block:
    raise SystemExit("failed to find [tokens.ZKSYS] in output-deploy-erc20.toml")
m = re.search(r'(?m)^address\s*=\s*"(0x[0-9a-fA-F]{40})"$', block.group(1))
if not m:
    raise SystemExit("failed to parse ZKSYS token address from output-deploy-erc20.toml")
print(m.group(1))
PY
)"

# NTV asset id formula: keccak256(abi.encode(chainId, L2_NATIVE_TOKEN_VAULT_ADDR, tokenAddress))
export L2_NATIVE_TOKEN_VAULT_ADDR=0x0000000000000000000000000000000000010004
export ZK_TOKEN_ASSET_ID="$(cast abi-encode \
  "f(uint256,address,address)" \
  "${SYSCOIN_L1_CHAIN_ID}" \
  "${L2_NATIVE_TOKEN_VAULT_ADDR}" \
  "${ZKSYS_L1_TOKEN_ADDRESS}" | cast keccak)"

python3 - <<'PY'
from pathlib import Path
import os, re
p = Path("script-config/config-deploy-ctm.toml")
s = p.read_text(encoding="utf-8")
line = f'zk_token_asset_id = "{os.environ["ZK_TOKEN_ASSET_ID"]}"'
if re.search(r'(?m)^zk_token_asset_id\s*=', s):
    s = re.sub(r'(?m)^zk_token_asset_id\s*=.*$', line, s)
else:
    if not s.endswith("\n"):
        s += "\n"
    s += line + "\n"
p.write_text(s, encoding="utf-8")
print(f"configured zk_token_asset_id={os.environ['ZK_TOKEN_ASSET_ID']}")
PY

cd "${GATEWAY_DIR}"

zkstack ecosystem init \
  --zksync-os \
  --update-submodules false \
  --l1-rpc-url ${L1_RPC_URL} \
  --deploy-ecosystem true \
  --deploy-erc20 false \
  --deploy-paymaster false \
  --ecosystem-only \
  --no-genesis \
  --observability false
```


If `ecosystem init` fails after partially deploying L1 contracts, deterministic Create2 addresses may
already be occupied on that L1 for the current `create2_factory_salt`. Preferred recovery is a clean
restart. If you must recover in-place, rotate `create2_factory_salt` in
`configs/initial_deployments.yaml` before retrying:

```bash
python3 - <<'PY'
from pathlib import Path
import secrets, re
p = Path("configs/initial_deployments.yaml")
s = p.read_text()
new_salt = "0x" + secrets.token_hex(32)
s = re.sub(r"(?m)^create2_factory_salt:\s*0x[0-9a-fA-F]+$", f"create2_factory_salt: {new_salt}", s)
p.write_text(s)
print(f"updated create2_factory_salt={new_salt}")
PY
```

## 3) Initialize Gateway chain

```bash
zkstack chain init \
  --chain gateway \
  --no-genesis \
  --deploy-paymaster false \
  --l1-rpc-url ${L1_RPC_URL}
```

## 4) Convert to Gateway settlement layer

```bash
zkstack chain gateway create-tx-filterer --chain gateway
zkstack chain gateway convert-to-gateway --chain gateway
```

## 5) Create and initialize child chain

```bash
export CHILD_CHAIN_NAME=zksys
export CHILD_CHAIN_ID=57057

zkstack chain create \
  --chain-name ${CHILD_CHAIN_NAME} \
  --chain-id ${CHILD_CHAIN_ID} \
  --prover-mode gpu \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode rollup \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default false \
  --evm-emulator false \
  --zksync-os

zkstack chain init \
  --chain ${CHILD_CHAIN_NAME} \
  --no-genesis \
  --deploy-paymaster false \
  --skip-priority-txs \
  --l1-rpc-url ${L1_RPC_URL}
```

## 6) Start Gateway runtime (required before migrate)

```bash
# REQUIRED: migrate/finalize needs a live Gateway RPC endpoint.
# Do not run migration commands until this process is running and healthy.
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-gateway.yaml &

# Wait until Gateway RPC responds before continuing.
until curl -sS http://127.0.0.1:3050 >/dev/null; do sleep 1; done
```

## 7) Migrate child to Gateway

```bash
# If Gateway RPC is remote, set it in chains/gateway/configs/general.yaml
# (api.web3_json_rpc.http_url) before running migrate/finalize.

zkstack chain gateway migrate-to-gateway \
  --chain ${CHILD_CHAIN_NAME} \
  --gateway-chain-name gateway \
  -v

zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain ${CHILD_CHAIN_NAME} \
  --gateway-chain-name gateway
```

## 8) Start child runtime after migration

```bash
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-child.yaml
```

## Runtime presets and overrides

Runtime defaults are code-defined in:

- `node/bin/src/config/mod.rs`

Operational presets are in this repo (review and modify):

- `config-presets/testnet-gateway.yaml`
- `config-presets/mainnet-gateway.yaml`
- `config-presets/testnet-child.yaml`
- `config-presets/mainnet-child.yaml`

Use presets first, then per-chain generated config, then local override:

```bash
# Gateway (testnet)
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-gateway.yaml

# Child (testnet)
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-child.yaml
```

RocksDB persistence for these runtime commands:

- default: `./db/node1`
- override via env var: `general_rocks_db_path=/path/to/db`
- or set `general.rocks_db_path` in your config override

Use `mainnet-gateway.yaml` / `mainnet-child.yaml` on mainnet.

## Provers

- Run Airbender stacks separately for Gateway and child.
- Do not use Era-only `zkstack prover` path for this topology.

## Known gotchas

- Always pass explicit flags (`--l1-network`, booleans like `--set-as-default true`).
- Do not use `-a` on `ecosystem create` / `chain create`.
- Keep deployer/governor sufficiently funded before each `init`; low-balance prompt paths can panic in non-interactive sessions.
- For the selected contracts SHA from `zksync-os-server/local-chains/<protocol_version>/versions.yaml`,
  set `FOUNDRY_EVM_VERSION=shanghai` before `zkstack ecosystem init` / `zkstack chain init`.
- `zkstack ecosystem create` may run `git submodule update --init --recursive` on `link-to-code`.
  If your `contracts` submodule has local patch edits, keep it clean for `ecosystem create`,
  then re-pin `contracts` to `REQUIRED_CONTRACTS_SHA` and re-run
  `apply-era-contracts-syscoin-patch.sh` immediately before `ecosystem init`.
- If contracts checkout by SHA fails on a fresh clone, fetch by object ID first:
  - `git -C "${ZKSYNC_ERA_PATH}/contracts" fetch origin "${REQUIRED_CONTRACTS_SHA}"`
  - then `git -C "${ZKSYNC_ERA_PATH}/contracts" checkout "${REQUIRED_CONTRACTS_SHA}"`
- If `ecosystem init` fails with opcode-related reverts, first confirm:
  - `git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD` matches `REQUIRED_CONTRACTS_SHA`.
  - `echo "${FOUNDRY_EVM_VERSION}"` is `shanghai` in the same shell where `zkstack` runs.
- Do not use Mainnet's `zk_token_asset_id` on Tanenbaum.
  Deploy `ZKSYS`, derive `zk_token_asset_id` with
  `keccak256(abi.encode(chainId, 0x0000000000000000000000000000000000010004, tokenAddress))`,
  and write it to `contracts/l1-contracts/script-config/config-deploy-ctm.toml` before `ecosystem init`.
- If `ecosystem init` fails later with:
  - `Function selector 'fd3ca9d3' not found in the ABI`
  - (`fd3ca9d3` = `governanceAcceptOwnerAggregated(address,address)`)
  this indicates a control-plane/contracts API mismatch:
  - upstream `zkstack` expects `governanceAcceptOwnerAggregated` in `deploy-scripts/AdminFunctions.s.sol`
  - current `syscoin/era-contracts` zkOS history uses `governanceAcceptOwner` / `governanceAcceptAdmin` and does not provide the aggregated entrypoint.
  - fix by pinning a `zkstack`/`zksync-era` revision compatible with the selected contracts API (do not patch ad-hoc in deployment).
