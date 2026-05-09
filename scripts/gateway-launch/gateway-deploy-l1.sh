#!/usr/bin/env bash
# §2: patch, build, genesis, dev contracts, ZKSYS erc20, CTM toml patch, zkstack ecosystem init --deploy-ecosystem.
# Requires: GATEWAY_DIR, ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, L1_RPC_URL, L1_CHAIN_ID,
#           REQUIRED_CONTRACTS_SHA, REQUIRED_ZKSTACK_CLI_SHA, optional FOUNDRY_EVM_VERSION
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

gl_export_foundry_evm_version
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
gl_l1_broadcast_preflight

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

CREATE2_FACTORY_SALT_FROM_CONFIG="$(python3 - <<'PY'
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
CREATE2_FACTORY_SALT="${CREATE2_FACTORY_SALT_FROM_CONFIG}"

if [ -n "${GATEWAY_CREATE2_FACTORY_SALT:-}" ]; then
  CREATE2_FACTORY_SALT="$(python3 - <<'PY'
import os
raw = str(os.environ["GATEWAY_CREATE2_FACTORY_SALT"]).strip()
if raw.startswith(("0x", "0X")):
    h = raw[2:]
    if len(h) == 0 or len(h) > 64:
        raise SystemExit("GATEWAY_CREATE2_FACTORY_SALT hex length must be 1..64 nybbles")
    v = int(h, 16)
else:
    v = int(raw, 10)
if v < 0 or v >= (1 << 256):
    raise SystemExit("GATEWAY_CREATE2_FACTORY_SALT must fit uint256")
print("0x" + format(v, "064x"))
PY
)"
  export CREATE2_FACTORY_SALT
  python3 - <<'PY'
import os, yaml
from pathlib import Path
p = Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml"
d = yaml.safe_load(p.read_text(encoding="utf-8")) or {}
d["create2_factory_salt"] = os.environ["CREATE2_FACTORY_SALT"]
p.write_text(yaml.safe_dump(d, sort_keys=False), encoding="utf-8")
PY
  echo "gateway-launch: using GATEWAY_CREATE2_FACTORY_SALT=${CREATE2_FACTORY_SALT}"
fi

CREATE2_FACTORY_ADDR="$(python3 - <<'PY'
import os, yaml
from pathlib import Path
d = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml").read_text())
addr = d.get("create2_factory_addr", "0x4e59b44847b379578588920cA78FbF26c0B4956C")
if isinstance(addr, int):
    v = addr
else:
    raw = str(addr).strip()
    if raw.startswith(("0x", "0X")):
        v = int(raw[2:], 16)
    elif raw.isdecimal():
        v = int(raw, 10)
    else:
        v = int(raw, 16)
if v < 0 or v >= (1 << 160):
    raise SystemExit("create2_factory_addr must fit address")
print("0x" + format(v, "040x"))
PY
)"

cast_code_or_die() {
  local addr="${1:?address required}"
  local code
  if ! code="$(cast code "${addr}" --rpc-url "${L1_RPC_URL}")"; then
    echo "failed to read code at ${addr}" >&2
    return 1
  fi
  [ -n "${code}" ] || {
    echo "empty code response for ${addr}" >&2
    return 1
  }
  printf '%s\n' "${code}"
}

address_has_code_or_die() {
  local addr="${1:?address required}"
  local code
  code="$(cast_code_or_die "${addr}")" || return 1
  [ "${code}" != "0x" ]
}

require_code_at() {
  local addr="${1:?address required}"
  local label="${2:?label required}"
  if ! address_has_code_or_die "${addr}"; then
    echo "${label} has no code at ${addr}" >&2
    exit 1
  fi
}

require_code_at "${CREATE2_FACTORY_ADDR}" "create2 factory"

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

GATEWAY_REUSE_ZKSYS_TOKEN="$(gl_to_lower "${GATEWAY_REUSE_ZKSYS_TOKEN:-false}")"
case "${GATEWAY_REUSE_ZKSYS_TOKEN}" in
true | false) ;;
*) gl_die "invalid GATEWAY_REUSE_ZKSYS_TOKEN='${GATEWAY_REUSE_ZKSYS_TOKEN}' (expected: true | false)" ;;
esac

KNOWN_ZKSYS_ADDRESS=""
if [ "${GATEWAY_REUSE_ZKSYS_TOKEN}" = true ]; then
  # SYSCOIN: reusing a token is an explicit recovery path only. Otherwise stale
  # script-out artifacts or ambient env must not control native-token binding.
  KNOWN_ZKSYS_ADDRESS="${ZKSYS_L1_TOKEN_ADDRESS:-}"
  if [ -z "${KNOWN_ZKSYS_ADDRESS}" ]; then
    KNOWN_ZKSYS_ADDRESS="$(extract_zksys_address_from_output || true)"
  fi
elif [ -n "${ZKSYS_L1_TOKEN_ADDRESS:-}" ]; then
  gl_die "ZKSYS_L1_TOKEN_ADDRESS requires GATEWAY_REUSE_ZKSYS_TOKEN=true"
fi

if [ -n "${KNOWN_ZKSYS_ADDRESS}" ]; then
  if ! address_has_code_or_die "${KNOWN_ZKSYS_ADDRESS}"; then
    gl_die "requested ZKSYS token reuse but no code was found at ${KNOWN_ZKSYS_ADDRESS}"
  fi
  export ZKSYS_L1_TOKEN_ADDRESS="${KNOWN_ZKSYS_ADDRESS}"
  echo "gateway-launch: explicitly reusing existing ZKSYS token at ${ZKSYS_L1_TOKEN_ADDRESS}; skipping DeployErc20"
elif [ "${GATEWAY_REUSE_ZKSYS_TOKEN}" = true ]; then
  gl_die "GATEWAY_REUSE_ZKSYS_TOKEN=true requires ZKSYS_L1_TOKEN_ADDRESS or script-out/output-deploy-erc20.toml"
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

    if [ -n "${ZKSYS_L1_TOKEN_ADDRESS}" ] && address_has_code_or_die "${ZKSYS_L1_TOKEN_ADDRESS}"; then
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

require_code_at "${ZKSYS_L1_TOKEN_ADDRESS}" "zksys token"

export L2_NATIVE_TOKEN_VAULT_ADDR=0x0000000000000000000000000000000000010004
DERIVED_ZK_TOKEN_ASSET_ID="$(cast abi-encode \
  "f(uint256,address,address)" \
  "${L1_CHAIN_ID}" \
  "${L2_NATIVE_TOKEN_VAULT_ADDR}" \
  "${ZKSYS_L1_TOKEN_ADDRESS}" | cast keccak)"
if [ -n "${ZK_TOKEN_ASSET_ID:-}" ] && [ "$(gl_to_lower "${ZK_TOKEN_ASSET_ID}")" != "$(gl_to_lower "${DERIVED_ZK_TOKEN_ASSET_ID}")" ]; then
  gl_die "ZK_TOKEN_ASSET_ID=${ZK_TOKEN_ASSET_ID} does not match derived ${DERIVED_ZK_TOKEN_ASSET_ID}"
fi
export ZK_TOKEN_ASSET_ID="${DERIVED_ZK_TOKEN_ASSET_ID}"
echo "gateway-launch: derived ZK_TOKEN_ASSET_ID=${ZK_TOKEN_ASSET_ID}"

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

extract_l1_contracts_dir_from_log() {
  python3 - "${1}" "${ZKSYNC_ERA_PATH}/contracts/l1-contracts" <<'PY'
import re
import sys
from pathlib import Path

p = Path(sys.argv[1])
expected = Path(sys.argv[2]).resolve(strict=True)
t = p.read_text(encoding="utf-8", errors="ignore")
m = re.search(r"Transactions saved to:\s*(/[^ \n]+/contracts/l1-contracts/broadcast/DeployL1CoreContracts\.s\.sol/\d+/run-latest\.json)", t)
if not m:
    raise SystemExit(0)
run_latest = Path(m.group(1)).resolve(strict=False)
l1_contracts_dir = run_latest.parents[3]
if l1_contracts_dir != expected:
    print(
        f"gateway-launch: ignoring forge resume path outside pinned checkout: {l1_contracts_dir}",
        file=sys.stderr,
    )
    raise SystemExit(0)
print(l1_contracts_dir)
PY
}

run_ecosystem_init_resume() {
  local l1_contracts_dir="${1}"
  local expected_l1_contracts_dir
  expected_l1_contracts_dir="$(python3 - "${ZKSYNC_ERA_PATH}/contracts/l1-contracts" <<'PY'
import sys
from pathlib import Path
print(Path(sys.argv[1]).resolve(strict=True))
PY
)"
  l1_contracts_dir="$(python3 - "${l1_contracts_dir}" <<'PY'
import sys
from pathlib import Path
print(Path(sys.argv[1]).resolve(strict=True))
PY
)"
  if [ "${l1_contracts_dir}" != "${expected_l1_contracts_dir}" ]; then
    gl_die "refusing forge resume outside pinned l1-contracts checkout: ${l1_contracts_dir}"
  fi
  (
    cd "${l1_contracts_dir}"
    forge script deploy-scripts/ecosystem/DeployL1CoreContracts.s.sol \
      --legacy \
      --ffi \
      --rpc-url "${L1_RPC_URL}" \
      --private-key "${DEPLOYER_PRIVATE_KEY}" \
      --broadcast \
      --resume
  )
}

ecosystem_contracts_ready() {
  local contracts_file bridgehub_addr bytecodes_addr
  contracts_file="${GATEWAY_DIR}/configs/contracts.yaml"
  [ -f "${contracts_file}" ] || return 1

  # SYSCOIN: contracts.yaml stores these deployment outputs under the current
  # zkstack schema sections, not a top-level contracts map.
  read -r bridgehub_addr bytecodes_addr < <(python3 - "${contracts_file}" <<'PY'
import sys, yaml
from pathlib import Path
p = Path(sys.argv[1])
d = yaml.safe_load(p.read_text(encoding="utf-8")) or {}
bridgehub = d.get("core_ecosystem_contracts", {}).get("bridgehub_proxy_addr", "")
bytecodes = d.get("zksync_os_ctm", {}).get("l1_bytecodes_supplier_addr", "")
print(bridgehub, bytecodes)
PY
)

  [ -n "${bridgehub_addr}" ] || return 1
  [ -n "${bytecodes_addr}" ] || return 1
  address_has_code_or_die "${bridgehub_addr}" || return 1
  address_has_code_or_die "${bytecodes_addr}" || return 1
  return 0
}

: "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS:=3}"
: "${GATEWAY_RETRY_GAS_BUMP_PCT:=20}"
normalize_uint() {
  local name="${1:?name required}"
  local raw="${2:?value required}"
  local max="${3:?max required}"
  python3 - "${name}" "${raw}" "${max}" <<'PY'
import sys

name, raw, max_raw = sys.argv[1:]
if not raw.isdecimal():
    raise SystemExit(f"{name} must be an unsigned decimal integer")
value = int(raw, 10)
max_value = int(max_raw, 10)
if value > max_value:
    raise SystemExit(f"{name} must be <= {max_value}")
print(value)
PY
}

GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS="$(
  normalize_uint GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}" 100
)"
GATEWAY_RETRY_GAS_BUMP_PCT="$(
  normalize_uint GATEWAY_RETRY_GAS_BUMP_PCT "${GATEWAY_RETRY_GAS_BUMP_PCT}" 10000
)"
LAST_L1_CONTRACTS_DIR=""

if ecosystem_contracts_ready; then
  # SYSCOIN: checkpoint repair/reruns can reach this step after L1 ecosystem
  # contracts were already deployed. Treat confirmed on-chain readiness as
  # idempotent success instead of rerunning one-time initialization.
  echo "gateway-launch: ecosystem contracts already present in configs/contracts.yaml and on-chain; skipping ecosystem init"
  exit 0
fi

set_retry_gas_price() {
  local attempt base_wei bump_pct bump_factor gas_price_wei
  attempt="$(normalize_uint "retry attempt" "${1}" "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}")"
  bump_pct="${GATEWAY_RETRY_GAS_BUMP_PCT}"
  base_wei="$(cast gas-price --rpc-url "${L1_RPC_URL}")"
  base_wei="$(normalize_uint "cast gas-price" "${base_wei}" 1000000000000000)"
  if [ "${base_wei}" -lt 1000000000 ]; then
    base_wei=1000000000
  fi
  if [ "${attempt}" -le 1 ]; then
    gas_price_wei="${base_wei}"
  else
    # Attempt N uses base * (1 + bump_pct*(N-1)/100) to satisfy replacement rules.
    bump_factor=$((100 + bump_pct * (attempt - 1)))
    gas_price_wei=$(( (base_wei * bump_factor + 99) / 100 ))
  fi
  export ETH_GAS_PRICE="${gas_price_wei}"
  export FORGE_GAS_PRICE="${gas_price_wei}"
  echo "gateway-launch: retry gas price set to ${gas_price_wei} wei (attempt ${attempt})"
}

attempt=1
while true; do
  echo "gateway-launch: ecosystem init attempt ${attempt}/${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}"
  set_retry_gas_price "${attempt}"
  tmp_log="$(mktemp)"
  set +e
  if [ "${attempt}" -gt 1 ] && [ -n "${LAST_L1_CONTRACTS_DIR}" ] && [ -d "${LAST_L1_CONTRACTS_DIR}" ]; then
    echo "gateway-launch: retrying DeployL1CoreContracts with forge --resume from ${LAST_L1_CONTRACTS_DIR}"
    run_ecosystem_init_resume "${LAST_L1_CONTRACTS_DIR}" 2>&1 | tee "${tmp_log}"
  else
    run_ecosystem_init_once 2>&1 | tee "${tmp_log}"
  fi
  ec="${PIPESTATUS[0]}"
  set -e

  current_l1_contracts_dir="$(extract_l1_contracts_dir_from_log "${tmp_log}" || true)"
  if [ -n "${current_l1_contracts_dir}" ] && [ -d "${current_l1_contracts_dir}" ]; then
    LAST_L1_CONTRACTS_DIR="${current_l1_contracts_dir}"
  fi

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
    "already known",
    "nativetokenvaultalreadyset()",
)
sys.exit(0 if any(sig in t for sig in retry_signals) else 1)
PY
  then
    rm -f "${tmp_log}"
    if ecosystem_contracts_ready; then
      echo "gateway-launch: ecosystem contracts already materialized on-chain despite retryable broadcast error; continuing"
      break
    fi
    if [ "${attempt}" -ge "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}" ]; then
      echo "gateway-launch: ecosystem init failed after ${attempt} retryable/idempotent attempts" >&2
      exit 1
    fi
    echo "gateway-launch: detected retryable/idempotent ecosystem init error; waiting for nonce sync before retry"
    wait_for_deployer_nonce_sync
    sleep 10
    attempt=$((attempt + 1))
    continue
  fi

  rm -f "${tmp_log}"
  exit "${ec}"
done
