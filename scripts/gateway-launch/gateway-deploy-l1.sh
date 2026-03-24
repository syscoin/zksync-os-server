#!/usr/bin/env bash
# §2: patch, build, genesis, dev contracts, ZKSYS erc20, CTM toml patch, zkstack ecosystem init --deploy-ecosystem.
# Requires: GATEWAY_DIR, ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, L1_RPC_URL, L1_CHAIN_ID,
#           REQUIRED_CONTRACTS_SHA, FOUNDRY_EVM_VERSION=shanghai
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require GATEWAY_DIR
gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
gl_assert_contracts_sha
gl_path_for_zkstack

export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-shanghai}"
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"

cd "${GATEWAY_DIR}"
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-era-contracts-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}/contracts"

cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
forge build --skip test

mkdir -p "${ZKSYNC_ERA_PATH}/etc/env/file_based"
cd "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen"
cargo run --release -- --output-file "${ZKSYNC_ERA_PATH}/etc/env/file_based/genesis.json"

cd "${GATEWAY_DIR}"
zkstack dev contracts

cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
export PERMANENT_VALUES_INPUT="/script-config/permanent-values.toml"
export TOKENS_CONFIG="/script-config/config-deploy-erc20.toml"

CREATE2_FACTORY_SALT="$(python3 - <<'PY'
import os, yaml
from pathlib import Path
s = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml").read_text())["create2_factory_salt"]
if isinstance(s, int):
    print("0x" + format(s, "064x"))
else:
    t = str(s).strip()
    print(t if t.startswith("0x") else "0x" + t)
PY
)"
CREATE2_FACTORY_ADDR="$(python3 - <<'PY'
import os, yaml
from pathlib import Path
d = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml").read_text())
print(d.get("create2_factory_addr", "0x4e59b44847b379578588920cA78FbF26c0B4956C"))
PY
)"
test "$(cast code "${CREATE2_FACTORY_ADDR}" --rpc-url "${L1_RPC_URL}")" != "0x" || {
  echo "create2 factory has no code at ${CREATE2_FACTORY_ADDR}" >&2
  exit 1
}

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
mint = 1000000000000000000
EOF

export DEPLOYER_PRIVATE_KEY="$(python3 - <<'PY'
import os, yaml
from pathlib import Path
pk = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "wallets.yaml").read_text())["deployer"]["private_key"]
print(format(pk, "x").zfill(64) if isinstance(pk, int) else str(pk).lower().removeprefix("0x").zfill(64))
PY
)"

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
block = re.search(r"(?ms)^\[tokens\.ZKSYS\]\s*(.*?)^\[", text + "\n[", re.MULTILINE)
m = re.search(r'(?m)^address\s*=\s*"(0x[0-9a-fA-F]{40})"$', block.group(1))
print(m.group(1))
PY
)"
export L2_NATIVE_TOKEN_VAULT_ADDR=0x0000000000000000000000000000000000010004
export ZK_TOKEN_ASSET_ID="$(cast abi-encode \
  "f(uint256,address,address)" \
  "${L1_CHAIN_ID}" \
  "${L2_NATIVE_TOKEN_VAULT_ADDR}" \
  "${ZKSYS_L1_TOKEN_ADDRESS}" | cast keccak)"

test -f script-config/config-deploy-ctm.toml || \
  cp deploy-script-config-template/config-deploy-ctm.toml script-config/config-deploy-ctm.toml

python3 - <<'PY'
from pathlib import Path
import os, re
p = Path("script-config/config-deploy-ctm.toml")
s = p.read_text(encoding="utf-8")
s = re.sub(r"(?m)^is_zk_sync_os\s*=.*$", "is_zk_sync_os = true", s)
if not re.search(r"(?m)^is_zk_sync_os\s*=", s):
    s = "is_zk_sync_os = true\n" + s
line = f'zk_token_asset_id = "{os.environ["ZK_TOKEN_ASSET_ID"]}"'
s = re.sub(r"(?m)^zk_token_asset_id\s*=.*$", line, s) if re.search(r"(?m)^zk_token_asset_id\s*=", s) else s.rstrip() + "\n" + line + "\n"
p.write_text(s, encoding="utf-8")
PY

cd "${GATEWAY_DIR}"
zkstack ecosystem init \
  --zksync-os \
  --update-submodules false \
  --l1-rpc-url "${L1_RPC_URL}" \
  --deploy-ecosystem true \
  --deploy-erc20 false \
  --deploy-paymaster false \
  --ecosystem-only \
  --no-genesis \
  --observability false
