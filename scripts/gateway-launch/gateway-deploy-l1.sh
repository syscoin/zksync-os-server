#!/usr/bin/env bash
# §2: patch, build, genesis, dev contracts, ZKSYS erc20, CTM toml patch, zkstack ecosystem init --deploy-ecosystem.
# Requires: GATEWAY_DIR, ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, L1_RPC_URL, L1_CHAIN_ID,
#           REQUIRED_CONTRACTS_SHA, REQUIRED_ZKSTACK_CLI_SHA, FOUNDRY_EVM_VERSION=shanghai
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
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_assert_contracts_sha
gl_assert_zksync_era_sha
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
export DEPLOYER_ADDRESS="$(cast wallet address --private-key "${DEPLOYER_PRIVATE_KEY}")"

wait_for_deployer_nonce_sync() {
  local timeout_s poll_s start now latest pending
  timeout_s="${GATEWAY_DEPLOYER_PENDING_TIMEOUT:-1800}"
  poll_s="${GATEWAY_DEPLOYER_PENDING_POLL:-5}"
  start="$(date +%s)"
  while true; do
    latest="$(cast nonce "${DEPLOYER_ADDRESS}" --block latest --rpc-url "${L1_RPC_URL}")"
    pending="$(cast nonce "${DEPLOYER_ADDRESS}" --block pending --rpc-url "${L1_RPC_URL}")"
    if [ "${latest}" = "${pending}" ]; then
      return 0
    fi
    now="$(date +%s)"
    if [ $((now - start)) -ge "${timeout_s}" ]; then
      echo "deployer nonce did not converge within timeout: latest=${latest} pending=${pending}" >&2
      return 1
    fi
    echo "waiting for deployer pending txs to clear: latest=${latest} pending=${pending}"
    sleep "${poll_s}"
  done
}

extract_zksys_address_from_output() {
  python3 - <<'PY'
import re
from pathlib import Path
path = Path("script-out/output-deploy-erc20.toml")
if not path.exists():
    raise SystemExit(0)
text = path.read_text(encoding="utf-8")
block = re.search(r"(?ms)^\[tokens\.ZKSYS\]\s*(.*?)^\[", text + "\n[", re.MULTILINE)
if not block:
    raise SystemExit(0)
m = re.search(r'(?m)^address\s*=\s*"(0x[0-9a-fA-F]{40})"$', block.group(1))
if not m:
    raise SystemExit(0)
print(m.group(1))
PY
}

KNOWN_ZKSYS_ADDRESS="${ZKSYS_L1_TOKEN_ADDRESS:-}"
if [ -z "${KNOWN_ZKSYS_ADDRESS}" ]; then
  KNOWN_ZKSYS_ADDRESS="$(extract_zksys_address_from_output || true)"
fi

if [ -n "${KNOWN_ZKSYS_ADDRESS}" ] && [ "$(cast code "${KNOWN_ZKSYS_ADDRESS}" --rpc-url "${L1_RPC_URL}")" != "0x" ]; then
  export ZKSYS_L1_TOKEN_ADDRESS="${KNOWN_ZKSYS_ADDRESS}"
  echo "gateway-launch: reusing existing ZKSYS token at ${ZKSYS_L1_TOKEN_ADDRESS}; skipping DeployErc20"
else
  : "${GATEWAY_DEPLOY_ERC20_TIMEOUT:=1800}"
  : "${GATEWAY_DEPLOY_ERC20_MAX_ATTEMPTS:=4}"
  deploy_erc20_attempt=1
  while true; do
    echo "gateway-launch: DeployErc20 attempt ${deploy_erc20_attempt}/${GATEWAY_DEPLOY_ERC20_MAX_ATTEMPTS}"
    tmp_erc20_log="$(mktemp)"
    set +e
    if command -v timeout >/dev/null 2>&1; then
      if [ "${L1_NETWORK:-}" = "tanenbaum" ] || [ "${L1_NETWORK:-}" = "mainnet" ]; then
        timeout "${GATEWAY_DEPLOY_ERC20_TIMEOUT}" \
          forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --private-key "${DEPLOYER_PRIVATE_KEY}" \
          --broadcast \
          --slow 2>&1 | tee "${tmp_erc20_log}"
      else
        timeout "${GATEWAY_DEPLOY_ERC20_TIMEOUT}" \
          forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --private-key "${DEPLOYER_PRIVATE_KEY}" \
          --broadcast 2>&1 | tee "${tmp_erc20_log}"
      fi
      erc20_ec="${PIPESTATUS[0]}"
    else
      if [ "${L1_NETWORK:-}" = "tanenbaum" ] || [ "${L1_NETWORK:-}" = "mainnet" ]; then
        forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --private-key "${DEPLOYER_PRIVATE_KEY}" \
          --broadcast \
          --slow 2>&1 | tee "${tmp_erc20_log}"
      else
        forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --private-key "${DEPLOYER_PRIVATE_KEY}" \
          --broadcast 2>&1 | tee "${tmp_erc20_log}"
      fi
      erc20_ec="${PIPESTATUS[0]}"
    fi
    set -e

    export ZKSYS_L1_TOKEN_ADDRESS="$(extract_zksys_address_from_output || true)"
    if [ "${erc20_ec}" -eq 0 ]; then
      rm -f "${tmp_erc20_log}"
      break
    fi

    if [ -n "${ZKSYS_L1_TOKEN_ADDRESS}" ] && [ "$(cast code "${ZKSYS_L1_TOKEN_ADDRESS}" --rpc-url "${L1_RPC_URL}")" != "0x" ]; then
      echo "gateway-launch: DeployErc20 exited non-zero (${erc20_ec}) but token is deployed at ${ZKSYS_L1_TOKEN_ADDRESS}; continuing"
      rm -f "${tmp_erc20_log}"
      break
    fi

    if python3 - "${tmp_erc20_log}" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
t = p.read_text(encoding="utf-8", errors="ignore").lower()
retry_signals = (
    "replacement transaction underpriced",
    "nonce too low",
    "eoa nonce changed unexpectedly while sending transactions",
)
sys.exit(0 if any(sig in t for sig in retry_signals) else 1)
PY
    then
      rm -f "${tmp_erc20_log}"
      if [ "${deploy_erc20_attempt}" -ge "${GATEWAY_DEPLOY_ERC20_MAX_ATTEMPTS}" ]; then
        echo "gateway-launch: DeployErc20 failed after ${deploy_erc20_attempt} attempts due to nonce/replacement retryable errors" >&2
        exit "${erc20_ec}"
      fi
      wait_for_deployer_nonce_sync
      deploy_erc20_attempt=$((deploy_erc20_attempt + 1))
      continue
    fi

    echo "gateway-launch: DeployErc20 failed (exit=${erc20_ec}) and no deployed token could be confirmed" >&2
    rm -f "${tmp_erc20_log}"
    exit "${erc20_ec}"
  done
fi

test "$(cast code "${ZKSYS_L1_TOKEN_ADDRESS}" --rpc-url "${L1_RPC_URL}")" != "0x" || {
  echo "zksys token has no code at ${ZKSYS_L1_TOKEN_ADDRESS}" >&2
  exit 1
}

export L2_NATIVE_TOKEN_VAULT_ADDR=0x0000000000000000000000000000000000010004
if [ -z "${ZK_TOKEN_ASSET_ID:-}" ]; then
  export ZK_TOKEN_ASSET_ID="$(cast abi-encode \
    "f(uint256,address,address)" \
    "${L1_CHAIN_ID}" \
    "${L2_NATIVE_TOKEN_VAULT_ADDR}" \
    "${ZKSYS_L1_TOKEN_ADDRESS}" | cast keccak)"
  echo "gateway-launch: derived ZK_TOKEN_ASSET_ID=${ZK_TOKEN_ASSET_ID}"
else
  echo "gateway-launch: using provided ZK_TOKEN_ASSET_ID=${ZK_TOKEN_ASSET_ID}"
fi

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

run_ecosystem_init_once() {
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
}

ecosystem_contracts_ready() {
  local parsed bridgehub bytecode_supplier
  parsed="$(python3 - <<'PY'
import os
from pathlib import Path
import yaml

p = Path(os.environ["GATEWAY_DIR"]) / "configs" / "contracts.yaml"
if not p.exists():
    print("|")
    raise SystemExit(0)
d = yaml.safe_load(p.read_text(encoding="utf-8")) or {}
bridgehub = (d.get("core_ecosystem_contracts") or {}).get("bridgehub_proxy_addr", "")
bytecode_supplier = (d.get("zksync_os_ctm") or {}).get("l1_bytecodes_supplier_addr", "")
print(f"{bridgehub}|{bytecode_supplier}")
PY
)"
  bridgehub="${parsed%%|*}"
  bytecode_supplier="${parsed#*|}"
  if [ -z "${bridgehub}" ] || [ -z "${bytecode_supplier}" ]; then
    return 1
  fi
  [ "$(cast code "${bridgehub}" --rpc-url "${L1_RPC_URL}")" != "0x" ] || return 1
  [ "$(cast code "${bytecode_supplier}" --rpc-url "${L1_RPC_URL}")" != "0x" ] || return 1
  return 0
}

: "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS:=3}"
attempt=1
while true; do
  echo "gateway-launch: ecosystem init attempt ${attempt}/${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}"
  tmp_log="$(mktemp)"
  set +e
  run_ecosystem_init_once 2>&1 | tee "${tmp_log}"
  ec="${PIPESTATUS[0]}"
  set -e

  if [ "${ec}" -eq 0 ]; then
    rm -f "${tmp_log}"
    break
  fi

  if python3 - "${tmp_log}" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
t = p.read_text(encoding="utf-8", errors="ignore").lower()
retry_signals = (
    "replacement transaction underpriced",
    "nonce too low",
    "eoa nonce changed unexpectedly while sending transactions",
)
sys.exit(0 if any(sig in t for sig in retry_signals) else 1)
PY
  then
    rm -f "${tmp_log}"
    if [ "${attempt}" -ge "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}" ]; then
      echo "gateway-launch: ecosystem init failed after ${attempt} attempts due to nonce/replacement retryable errors" >&2
      exit 1
    fi
    echo "gateway-launch: detected nonce/replacement retryable error; waiting for nonce sync before retry"
    wait_for_deployer_nonce_sync
    attempt=$((attempt + 1))
    continue
  fi

  if python3 - "${tmp_log}" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
t = p.read_text(encoding="utf-8", errors="ignore").lower()
sys.exit(0 if "nativetokenvaultalreadyset()" in t else 1)
PY
  then
    rm -f "${tmp_log}"
    if ecosystem_contracts_ready; then
      echo "gateway-launch: NativeTokenVaultAlreadySet detected, but ecosystem contracts are present on-chain; continuing"
      break
    fi
    echo "gateway-launch: NativeTokenVaultAlreadySet encountered before ecosystem contracts were fully materialized" >&2
    exit "${ec}"
  fi

  rm -f "${tmp_log}"
  exit "${ec}"
done
