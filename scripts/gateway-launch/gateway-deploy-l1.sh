#!/usr/bin/env bash
# §2: patch, build, genesis, dev contracts, zkstack ecosystem init --deploy-ecosystem.
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

gl_ensure_zkstack_cli_release_current

cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
forge build --skip test

mkdir -p "${ZKSYNC_ERA_PATH}/etc/env/file_based"
cd "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen"
cargo run --release -- --output-file "${ZKSYNC_ERA_PATH}/etc/env/file_based/genesis.json"

cd "${GATEWAY_DIR}"
gl_zkstack_pty zkstack dev contracts

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

case "${L1_CHAIN_ID}" in
5700)
  : "${L1_WETH_TOKEN_ADDRESS:=0xa66b2E50c2b805F31712beA422D0D9e7D0Fd0F35}"
  ;;
57)
  : "${L1_WETH_TOKEN_ADDRESS:=0xd3e822f3ef011Ca5f17D82C956D952D8d7C3A1BB}"
  ;;
*)
  : "${L1_WETH_TOKEN_ADDRESS:=}"
  ;;
esac

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

normalize_zksys_bytes32_var() {
  local name="${1:?name required}"
  local default_value="${2:?default required}"
  python3 - "${name}" "${default_value}" <<'PY'
import os
import sys

name, default = sys.argv[1:]
raw = os.environ.get(name, default).strip()
if raw.startswith(("0x", "0X")):
    value = int(raw[2:] or "0", 16)
elif raw.isdecimal():
    value = int(raw, 10)
else:
    value = int(raw, 16)
if value < 0 or value >= 1 << 256:
    raise SystemExit(f"{name} must fit bytes32")
print("0x" + format(value, "064x"))
PY
}

normalize_zksys_address_var() {
  local name="${1:?name required}"
  python3 - "${name}" "${!name:-}" <<'PY'
import sys

name, raw = sys.argv[1:]
addr = raw.strip()
if not addr.startswith(("0x", "0X")) or len(addr) != 42:
    raise SystemExit(f"{name} must be a 20-byte hex address")
print("0x" + format(int(addr[2:], 16), "040x"))
PY
}

normalize_zksys_uint_var() {
  local name="${1:?name required}"
  local default_value="${2:?default required}"
  local max_value="${3:?max required}"
  python3 - "${name}" "${default_value}" "${max_value}" <<'PY'
import os
import sys

name, default, max_raw = sys.argv[1:]
raw = os.environ.get(name, default).strip()
if not raw.isdecimal():
    raise SystemExit(f"{name} must be an unsigned decimal integer")
value = int(raw, 10)
max_value = int(max_raw, 10)
if value > max_value:
    raise SystemExit(f"{name} must be <= {max_value}")
print(value)
PY
}

forge_inspect_zksys_bytecode() {
  local contract="${1:?contract required}"
  forge inspect "${contract}" bytecode \
    --no-metadata \
    --root "${ZKSYNC_OS_SERVER_PATH}/contracts" \
    -R "@openzeppelin/contracts/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/openzeppelin-contracts/contracts/" \
    -R "@openzeppelin/contracts-v4/=${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-v4/contracts/" \
    -R "@openzeppelin/contracts-upgradeable-v4/=${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts/" \
    -R "@openzeppelin/community-contracts/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/openzeppelin-community-contracts/contracts/" \
    -R "forge-std/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/forge-std/src/"
}

derive_and_export_zksys_zk_token_asset_id() {
  if [ "$(gl_to_lower "${L1_NETWORK:-}")" != "mainnet" ]; then
    return 0
  fi

  gl_require GATEWAY_CHAIN_ID
  gl_require ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  [ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-v4/contracts" ] ||
    gl_die "missing OpenZeppelin v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"
  [ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts" ] ||
    gl_die "missing OpenZeppelin upgradeable v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"

  local create2_deployer proxy_admin_salt token_impl_salt token_proxy_salt
  local proxy_admin_ctor_args proxy_admin_init_code proxy_admin_address
  local token_impl_init_code token_impl_address token_init_data token_proxy_ctor_args token_proxy_init_code
  local token_address encoded_asset_id_inputs

  create2_deployer="${ZKSYS_L2_CREATE2_DEPLOYER:-0x4e59b44847b379578588920cA78FbF26c0B4956C}"
  export ZKSYS_L2_CREATE2_DEPLOYER="${create2_deployer}"
  create2_deployer="$(normalize_zksys_address_var ZKSYS_L2_CREATE2_DEPLOYER)"
  ZKSYS_L2_TOKEN_ADMIN_ADDRESS="$(normalize_zksys_address_var ZKSYS_L2_TOKEN_ADMIN_ADDRESS)"
  [ "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" != "0x0000000000000000000000000000000000000000" ] ||
    gl_die "ZKSYS_L2_TOKEN_ADMIN_ADDRESS must not be zero"
  export ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  proxy_admin_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_PROXY_ADMIN_SALT 0x7a6b7379732d70726f78792d61646d696e000000000000000000000000000000)"
  token_impl_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_TOKEN_IMPL_SALT 0x7a6b7379732d746f6b656e2d696d706c00000000000000000000000000000000)"
  token_proxy_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_TOKEN_PROXY_SALT 0x7a6b7379732d746f6b656e2d70726f7879000000000000000000000000000000)"

  proxy_admin_ctor_args="$(cast abi-encode "constructor(address)" "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}")"
  proxy_admin_init_code="$(forge_inspect_zksys_bytecode ZkSysProxyAdmin)${proxy_admin_ctor_args#0x}"
  proxy_admin_address="$(
    cast create2 \
      --deployer "${create2_deployer}" \
      --salt "${proxy_admin_salt}" \
      --init-code "${proxy_admin_init_code}"
  )"

  token_impl_init_code="$(forge_inspect_zksys_bytecode SyscoinZKSYSToken)"
  token_impl_address="$(
    cast create2 \
      --deployer "${create2_deployer}" \
      --salt "${token_impl_salt}" \
      --init-code "${token_impl_init_code}"
  )"

  token_init_data="$(
    cast calldata \
      "initialize(string,string,uint8,address)" \
      "${ZKSYS_L2_TOKEN_NAME:-ZKSYS}" \
      "${ZKSYS_L2_TOKEN_SYMBOL:-ZKSYS}" \
      "${ZKSYS_L2_TOKEN_DECIMALS:-18}" \
      "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}"
  )"
  token_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${token_impl_address}" "${proxy_admin_address}" "${token_init_data}")"
  token_proxy_init_code="$(forge_inspect_zksys_bytecode ZkSysCreate2ProxyBytecode)${token_proxy_ctor_args#0x}"
  token_address="$(
    cast create2 \
      --deployer "${create2_deployer}" \
      --salt "${token_proxy_salt}" \
      --init-code "${token_proxy_init_code}"
  )"

  # v31 InteropCenter resolves the fixed-fee token via
  # L2NativeTokenVault.tokenAddress(keccak256(abi.encode(originChainId, L2_NTV, token))).
  encoded_asset_id_inputs="$(
    cast abi-encode \
      "constructor(uint256,address,address)" \
      "${GATEWAY_CHAIN_ID}" \
      "0x0000000000000000000000000000000000010004" \
      "${token_address}"
  )"
  ZKSYS_ZK_TOKEN_ASSET_ID="$(cast keccak "${encoded_asset_id_inputs}")"
  export ZKSYS_ZK_TOKEN_ASSET_ID
  export ZK_TOKEN_ASSET_ID="${ZKSYS_ZK_TOKEN_ASSET_ID}"
  echo "gateway-launch: derived zkSYS L2 token address ${token_address}"
  echo "gateway-launch: derived ZKSYS_ZK_TOKEN_ASSET_ID=${ZKSYS_ZK_TOKEN_ASSET_ID}"
}

derive_zksys_l2_registry_address() {
  gl_require ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  [ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-v4/contracts" ] ||
    gl_die "missing OpenZeppelin v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"
  [ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts" ] ||
    gl_die "missing OpenZeppelin upgradeable v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"

  local create2_deployer proxy_admin_salt registry_impl_salt registry_proxy_salt
  local proxy_admin_ctor_args proxy_admin_init_code proxy_admin_address
  local registry_impl_init_code registry_impl_address registry_init_data registry_proxy_ctor_args registry_proxy_init_code

  create2_deployer="${ZKSYS_L2_CREATE2_DEPLOYER:-0x4e59b44847b379578588920cA78FbF26c0B4956C}"
  export ZKSYS_L2_CREATE2_DEPLOYER="${create2_deployer}"
  create2_deployer="$(normalize_zksys_address_var ZKSYS_L2_CREATE2_DEPLOYER)"
  ZKSYS_L2_TOKEN_ADMIN_ADDRESS="$(normalize_zksys_address_var ZKSYS_L2_TOKEN_ADMIN_ADDRESS)"
  [ "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" != "0x0000000000000000000000000000000000000000" ] ||
    gl_die "ZKSYS_L2_TOKEN_ADMIN_ADDRESS must not be zero"
  export ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  proxy_admin_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_PROXY_ADMIN_SALT 0x7a6b7379732d70726f78792d61646d696e000000000000000000000000000000)"
  registry_impl_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_REGISTRY_IMPL_SALT 0x7a6b7379732d72656769737472792d696d706c00000000000000000000000000)"
  registry_proxy_salt="$(normalize_zksys_bytes32_var ZKSYS_L2_REGISTRY_PROXY_SALT 0x7a6b7379732d72656769737472792d70726f7879000000000000000000000000)"

  proxy_admin_ctor_args="$(cast abi-encode "constructor(address)" "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}")"
  proxy_admin_init_code="$(forge_inspect_zksys_bytecode ZkSysProxyAdmin)${proxy_admin_ctor_args#0x}"
  proxy_admin_address="$(
    cast create2 \
      --deployer "${create2_deployer}" \
      --salt "${proxy_admin_salt}" \
      --init-code "${proxy_admin_init_code}"
  )"

  registry_impl_init_code="$(forge_inspect_zksys_bytecode ZkSysMembershipRegistry)"
  registry_impl_address="$(
    cast create2 \
      --deployer "${create2_deployer}" \
      --salt "${registry_impl_salt}" \
      --init-code "${registry_impl_init_code}"
  )"

  registry_init_data="$(
    cast calldata \
      "initialize(address,address)" \
      "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
      "0x0000000000000000000000000000000000000000"
  )"
  registry_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${registry_impl_address}" "${proxy_admin_address}" "${registry_init_data}")"
  registry_proxy_init_code="$(forge_inspect_zksys_bytecode ZkSysCreate2ProxyBytecode)${registry_proxy_ctor_args#0x}"
  cast create2 \
    --deployer "${create2_deployer}" \
    --salt "${registry_proxy_salt}" \
    --init-code "${registry_proxy_init_code}"
}

get_l1_bridgehub_proxy_addr() {
  python3 - "${GATEWAY_DIR}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
addr = data.get("core_ecosystem_contracts", {}).get("bridgehub_proxy_addr", "")
if not isinstance(addr, str) or not addr.startswith(("0x", "0X")) or len(addr) != 42:
    raise SystemExit(f"missing core_ecosystem_contracts.bridgehub_proxy_addr in {path}")
print("0x" + format(int(addr[2:], 16), "040x"))
PY
}

persist_zksys_l1_registry_bridge_address() {
  local address="${1:?address required}"
  python3 - "${GATEWAY_DIR}/configs/contracts.yaml" "${address}" <<'PY'
import sys
import tempfile
from pathlib import Path

import yaml

path = Path(sys.argv[1])
address = sys.argv[2]
data = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
section = data.setdefault("zksys", {})
section["l1_registry_bridge_addr"] = address

with tempfile.NamedTemporaryFile("w", delete=False, dir=path.parent, encoding="utf-8") as tmp:
    yaml.safe_dump(data, tmp, sort_keys=False)
    tmp_name = tmp.name
Path(tmp_name).replace(path)
PY
}

deploy_zksys_l1_registry_bridge() {
  : "${ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE:=true}"
  case "$(gl_to_lower "${ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE}")" in
  true) ;;
  false)
    echo "gateway-launch: skipping zkSYS L1 registry bridge deployment"
    return 0
    ;;
  *) gl_die "ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE must be true or false" ;;
  esac

  gl_require ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  local bridgehub zksys_chain_id l2_registry nevm_start_block seniority_height1 seniority_height2
  local seniority_level1_bps seniority_level2_bps proxy_admin_owner
  local proxy_admin_salt bridge_impl_salt bridge_proxy_salt
  local proxy_admin_ctor_args proxy_admin_init_code proxy_admin_address
  local bridge_impl_init_code bridge_impl_address bridge_init_data bridge_proxy_ctor_args bridge_proxy_init_code
  local expected_address code
  local actual_proxy_admin_owner actual_bridge_proxy_admin actual_bridge_impl
  local actual_bridgehub actual_chain_id actual_l2_registry actual_nevm_start_block
  local actual_seniority_height1 actual_seniority_height2 actual_seniority_level1_bps actual_seniority_level2_bps

  ZKSYS_L2_TOKEN_ADMIN_ADDRESS="$(normalize_zksys_address_var ZKSYS_L2_TOKEN_ADMIN_ADDRESS)"
  [ "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" != "0x0000000000000000000000000000000000000000" ] ||
    gl_die "ZKSYS_L2_TOKEN_ADMIN_ADDRESS must not be zero"
  export ZKSYS_L2_TOKEN_ADMIN_ADDRESS

  bridgehub="$(get_l1_bridgehub_proxy_addr)"
  zksys_chain_id="$(normalize_zksys_uint_var EDGE_CHAIN_ID "${EDGE_CHAIN_ID:-57057}" 18446744073709551615)"
  l2_registry="$(derive_zksys_l2_registry_address)"
  nevm_start_block="$(normalize_zksys_uint_var ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK 1317500 4294967295)"
  seniority_height1="$(normalize_zksys_uint_var ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1 210240 4294967295)"
  seniority_height2="$(normalize_zksys_uint_var ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT2 525600 4294967295)"
  seniority_level1_bps="$(normalize_zksys_uint_var ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS 0 65535)"
  seniority_level2_bps="$(normalize_zksys_uint_var ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS 0 65535)"
  proxy_admin_owner="${ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS:-${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}}"
  export ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS="${proxy_admin_owner}"
  proxy_admin_owner="$(normalize_zksys_address_var ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS)"
  [ "${proxy_admin_owner}" != "0x0000000000000000000000000000000000000000" ] ||
    gl_die "ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS must not be zero"

  if [ "${seniority_height1}" = "0" ] || [ "${seniority_height2}" -le "${seniority_height1}" ] ||
    [ "${seniority_level2_bps}" -lt "${seniority_level1_bps}" ]; then
    gl_die "invalid zkSYS L1 registry bridge seniority config"
  fi

  proxy_admin_salt="$(normalize_zksys_bytes32_var ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_SALT 0x7a6b7379732d6c312d72656769737472792d6272696467652d61646d696e0000)"
  bridge_impl_salt="$(normalize_zksys_bytes32_var ZKSYS_L1_REGISTRY_BRIDGE_IMPL_SALT 0x7a6b7379732d6c312d72656769737472792d6272696467652d696d706c000000)"
  bridge_proxy_salt="$(normalize_zksys_bytes32_var ZKSYS_L1_REGISTRY_BRIDGE_PROXY_SALT 0x7a6b7379732d6c312d72656769737472792d6272696467652d70726f78790000)"

  proxy_admin_ctor_args="$(cast abi-encode "constructor(address)" "${proxy_admin_owner}")"
  proxy_admin_init_code="$(forge_inspect_zksys_bytecode ZkSysProxyAdmin)${proxy_admin_ctor_args#0x}"
  proxy_admin_address="$(
    cast create2 \
      --deployer "${CREATE2_FACTORY_ADDR}" \
      --salt "${proxy_admin_salt}" \
      --init-code "${proxy_admin_init_code}"
  )"

  bridge_impl_init_code="$(forge_inspect_zksys_bytecode ZkSysRegistryBridge)"
  bridge_impl_address="$(
    cast create2 \
      --deployer "${CREATE2_FACTORY_ADDR}" \
      --salt "${bridge_impl_salt}" \
      --init-code "${bridge_impl_init_code}"
  )"

  bridge_init_data="$(
    cast calldata \
      "initialize(address,uint256,address,uint32,uint32,uint32,uint16,uint16)" \
      "${bridgehub}" \
      "${zksys_chain_id}" \
      "${l2_registry}" \
      "${nevm_start_block}" \
      "${seniority_height1}" \
      "${seniority_height2}" \
      "${seniority_level1_bps}" \
      "${seniority_level2_bps}"
  )"
  bridge_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${bridge_impl_address}" "${proxy_admin_address}" "${bridge_init_data}")"
  bridge_proxy_init_code="$(forge_inspect_zksys_bytecode ZkSysCreate2ProxyBytecode)${bridge_proxy_ctor_args#0x}"
  expected_address="$(
    cast create2 \
      --deployer "${CREATE2_FACTORY_ADDR}" \
      --salt "${bridge_proxy_salt}" \
      --init-code "${bridge_proxy_init_code}"
  )"

  code="$(cast_code_or_die "${proxy_admin_address}")"
  if [ "${code}" = "0x" ]; then
    echo "gateway-launch: deploying zkSYS L1 registry bridge proxy admin to ${proxy_admin_address}"
    cast send \
      --rpc-url "${L1_RPC_URL}" \
      "${DEPLOYER_CAST_WALLET_ARGS[@]}" \
      "${CREATE2_FACTORY_ADDR}" \
      "${proxy_admin_salt}${proxy_admin_init_code#0x}" >/dev/null
    code="$(cast_code_or_die "${proxy_admin_address}")"
    [ "${code}" != "0x" ] || gl_die "zkSYS L1 registry bridge proxy admin deployment did not create code at ${proxy_admin_address}"
  else
    echo "gateway-launch: zkSYS L1 registry bridge proxy admin already deployed at ${proxy_admin_address}"
  fi

  code="$(cast_code_or_die "${bridge_impl_address}")"
  if [ "${code}" = "0x" ]; then
    echo "gateway-launch: deploying zkSYS L1 registry bridge implementation to ${bridge_impl_address}"
    cast send \
      --rpc-url "${L1_RPC_URL}" \
      "${DEPLOYER_CAST_WALLET_ARGS[@]}" \
      "${CREATE2_FACTORY_ADDR}" \
      "${bridge_impl_salt}${bridge_impl_init_code#0x}" >/dev/null
    code="$(cast_code_or_die "${bridge_impl_address}")"
    [ "${code}" != "0x" ] || gl_die "zkSYS L1 registry bridge implementation deployment did not create code at ${bridge_impl_address}"
  else
    echo "gateway-launch: zkSYS L1 registry bridge implementation already deployed at ${bridge_impl_address}"
  fi

  code="$(cast_code_or_die "${expected_address}")"
  if [ "${code}" = "0x" ]; then
    echo "gateway-launch: deploying zkSYS L1 registry bridge proxy to ${expected_address}"
    cast send \
      --rpc-url "${L1_RPC_URL}" \
      "${DEPLOYER_CAST_WALLET_ARGS[@]}" \
      "${CREATE2_FACTORY_ADDR}" \
      "${bridge_proxy_salt}${bridge_proxy_init_code#0x}" >/dev/null
    code="$(cast_code_or_die "${expected_address}")"
    [ "${code}" != "0x" ] || gl_die "zkSYS L1 registry bridge proxy deployment did not create code at ${expected_address}"
  else
    echo "gateway-launch: zkSYS L1 registry bridge proxy already deployed at ${expected_address}"
  fi

  actual_proxy_admin_owner="$(cast call "${proxy_admin_address}" "owner()(address)" --rpc-url "${L1_RPC_URL}")"
  [ "$(gl_to_lower "${actual_proxy_admin_owner}")" = "$(gl_to_lower "${proxy_admin_owner}")" ] ||
    gl_die "zkSYS L1 registry bridge proxy admin owner mismatch: ${actual_proxy_admin_owner} != ${proxy_admin_owner}"

  actual_bridge_proxy_admin="$(cast call "${proxy_admin_address}" "getProxyAdmin(address)(address)" "${expected_address}" --rpc-url "${L1_RPC_URL}")"
  [ "$(gl_to_lower "${actual_bridge_proxy_admin}")" = "$(gl_to_lower "${proxy_admin_address}")" ] ||
    gl_die "zkSYS L1 registry bridge proxy admin mismatch: ${actual_bridge_proxy_admin} != ${proxy_admin_address}"

  actual_bridge_impl="$(cast call "${proxy_admin_address}" "getProxyImplementation(address)(address)" "${expected_address}" --rpc-url "${L1_RPC_URL}")"
  [ "$(gl_to_lower "${actual_bridge_impl}")" = "$(gl_to_lower "${bridge_impl_address}")" ] ||
    gl_die "zkSYS L1 registry bridge implementation mismatch: ${actual_bridge_impl} != ${bridge_impl_address}"

  actual_bridgehub="$(cast call "${expected_address}" "bridgehub()(address)" --rpc-url "${L1_RPC_URL}")"
  actual_chain_id="$(cast call "${expected_address}" "zksysChainId()(uint256)" --rpc-url "${L1_RPC_URL}")"
  actual_l2_registry="$(cast call "${expected_address}" "l2Registry()(address)" --rpc-url "${L1_RPC_URL}")"
  actual_nevm_start_block="$(cast call "${expected_address}" "nevmStartBlock()(uint32)" --rpc-url "${L1_RPC_URL}")"
  actual_seniority_height1="$(cast call "${expected_address}" "seniorityHeight1()(uint32)" --rpc-url "${L1_RPC_URL}")"
  actual_seniority_height2="$(cast call "${expected_address}" "seniorityHeight2()(uint32)" --rpc-url "${L1_RPC_URL}")"
  actual_seniority_level1_bps="$(cast call "${expected_address}" "seniorityLevel1Bps()(uint16)" --rpc-url "${L1_RPC_URL}")"
  actual_seniority_level2_bps="$(cast call "${expected_address}" "seniorityLevel2Bps()(uint16)" --rpc-url "${L1_RPC_URL}")"

  [ "$(gl_to_lower "${actual_bridgehub}")" = "$(gl_to_lower "${bridgehub}")" ] ||
    gl_die "zkSYS L1 registry bridge bridgehub mismatch: ${actual_bridgehub} != ${bridgehub}"
  [ "${actual_chain_id}" = "${zksys_chain_id}" ] ||
    gl_die "zkSYS L1 registry bridge chain id mismatch: ${actual_chain_id} != ${zksys_chain_id}"
  [ "$(gl_to_lower "${actual_l2_registry}")" = "$(gl_to_lower "${l2_registry}")" ] ||
    gl_die "zkSYS L1 registry bridge L2 registry mismatch: ${actual_l2_registry} != ${l2_registry}"
  [ "${actual_nevm_start_block}" = "${nevm_start_block}" ] ||
    gl_die "zkSYS L1 registry bridge NEVM start mismatch: ${actual_nevm_start_block} != ${nevm_start_block}"
  [ "${actual_seniority_height1}" = "${seniority_height1}" ] ||
    gl_die "zkSYS L1 registry bridge seniority height1 mismatch: ${actual_seniority_height1} != ${seniority_height1}"
  [ "${actual_seniority_height2}" = "${seniority_height2}" ] ||
    gl_die "zkSYS L1 registry bridge seniority height2 mismatch: ${actual_seniority_height2} != ${seniority_height2}"
  [ "${actual_seniority_level1_bps}" = "${seniority_level1_bps}" ] ||
    gl_die "zkSYS L1 registry bridge seniority level1 mismatch: ${actual_seniority_level1_bps} != ${seniority_level1_bps}"
  [ "${actual_seniority_level2_bps}" = "${seniority_level2_bps}" ] ||
    gl_die "zkSYS L1 registry bridge seniority level2 mismatch: ${actual_seniority_level2_bps} != ${seniority_level2_bps}"

  ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS="${expected_address}"
  export ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS
  persist_zksys_l1_registry_bridge_address "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}"
  echo "gateway-launch: zkSYS L1 registry bridge proxy admin: ${proxy_admin_address} owner=${proxy_admin_owner}"
  echo "gateway-launch: zkSYS L1 registry bridge implementation: ${bridge_impl_address}"
  echo "gateway-launch: zkSYS L1 registry bridge ready at ${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}"
  echo "gateway-launch: zkSYS L1 registry bridge params: chain=${zksys_chain_id}, l2Registry=${l2_registry}, nevmStartBlock=${nevm_start_block}, seniority=${seniority_height1}/${seniority_height2}, bps=${seniority_level1_bps}/${seniority_level2_bps}"
}

if [ -n "${L1_WETH_TOKEN_ADDRESS}" ]; then
  require_code_at "${L1_WETH_TOKEN_ADDRESS}" "L1 wrapped native token"
  export L1_WETH_TOKEN_ADDRESS
  python3 - <<'PY'
import os
from pathlib import Path

import yaml

addr = os.environ["L1_WETH_TOKEN_ADDRESS"].strip()
if not addr.startswith(("0x", "0X")) or len(addr) != 42:
    raise SystemExit("L1_WETH_TOKEN_ADDRESS must be a 20-byte hex address")
int(addr[2:], 16)

path = Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml"
config = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
config["token_weth_address"] = addr
path.write_text(yaml.safe_dump(config, sort_keys=False), encoding="utf-8")
print(f"gateway-launch: wrote {path} token_weth_address={addr}")
PY
fi

require_code_at "${CREATE2_FACTORY_ADDR}" "create2 factory"
derive_and_export_zksys_zk_token_asset_id

cat > script-config/permanent-values.toml <<EOF
[permanent_contracts]
create2_factory_salt = "${CREATE2_FACTORY_SALT}"
create2_factory_addr = "${CREATE2_FACTORY_ADDR}"
EOF

if [ "$(gl_to_lower "${L1_NETWORK:-}")" = "mainnet" ]; then
  cat > script-config/config-deploy-erc20.toml <<'EOF'
# ZKSYS is canonical on L2. L1 representation is created by the native bridge
# when L2-origin zkSYS exits to L1, so no canonical L1 ERC20 is deployed here.
EOF
else
  cat > script-config/config-deploy-erc20.toml <<'EOF'
additional_addresses_for_minting = []

[tokens.ZKSYS]
name = "ZKSYS"
symbol = "ZKSYS"
decimals = 18
implementation = "TestnetERC20Token.sol"
mint = 1000000000000000000
EOF
fi

read_deployer_private_key() {
  python3 - <<'PY'
import os, yaml
from pathlib import Path
pk = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "wallets.yaml").read_text())["deployer"]["private_key"]
print(format(pk, "x").zfill(64) if isinstance(pk, int) else str(pk).lower().removeprefix("0x").zfill(64))
PY
}

DEPLOYER_FORGE_WALLET_ARGS=()
DEPLOYER_CAST_WALLET_ARGS=()

prepare_deployer_wallet_args() {
  local deployer_signer password_file deployer_private_key
  if [ -z "${DEPLOYER_SIGNER:-}" ]; then
    if gl_l1_network_requires_external_signer; then
      DEPLOYER_SIGNER="${FUNDER_SIGNER:-account}"
    else
      DEPLOYER_SIGNER="private-key"
    fi
  fi
  deployer_signer="$(gl_to_lower "${DEPLOYER_SIGNER}")"

  DEPLOYER_FORGE_WALLET_ARGS=()
  DEPLOYER_CAST_WALLET_ARGS=()

  case "${deployer_signer}" in
  private-key)
    if gl_l1_network_requires_external_signer && ! gl_allow_insecure_private_key_argv; then
      gl_die "DEPLOYER_SIGNER=private-key is not allowed on ${L1_NETWORK}; import the deployer into a Foundry account/keystore, use hardware/KMS signing, or set GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true for an explicit unsafe override"
    fi
    deployer_private_key="$(read_deployer_private_key)"
    DEPLOYER_FORGE_WALLET_ARGS+=(--private-key "${deployer_private_key}")
    DEPLOYER_CAST_WALLET_ARGS+=(--private-key "${deployer_private_key}")
    ;;
  account)
    local account_name="${DEPLOYER_ACCOUNT_NAME:-${FUNDER_ACCOUNT_NAME:-funder}}"
    [ -n "${account_name}" ] || gl_die "DEPLOYER_ACCOUNT_NAME must not be empty"
    DEPLOYER_FORGE_WALLET_ARGS+=(--account "${account_name}")
    DEPLOYER_CAST_WALLET_ARGS+=(--account "${account_name}")
    ;;
  keystore)
    local keystore_path="${DEPLOYER_KEYSTORE:-${FUNDER_KEYSTORE:-}}"
    [ -n "${keystore_path}" ] || gl_die "DEPLOYER_KEYSTORE is required when DEPLOYER_SIGNER=keystore"
    [ -f "${keystore_path}" ] || gl_die "deployer keystore does not exist: ${keystore_path}"
    DEPLOYER_FORGE_WALLET_ARGS+=(--keystore "${keystore_path}")
    DEPLOYER_CAST_WALLET_ARGS+=(--keystore "${keystore_path}")
    ;;
  ledger)
    DEPLOYER_FORGE_WALLET_ARGS+=(--ledger)
    DEPLOYER_CAST_WALLET_ARGS+=(--ledger)
    ;;
  trezor)
    DEPLOYER_FORGE_WALLET_ARGS+=(--trezor)
    DEPLOYER_CAST_WALLET_ARGS+=(--trezor)
    ;;
  aws)
    DEPLOYER_FORGE_WALLET_ARGS+=(--aws)
    DEPLOYER_CAST_WALLET_ARGS+=(--aws)
    ;;
  gcp)
    DEPLOYER_FORGE_WALLET_ARGS+=(--gcp)
    DEPLOYER_CAST_WALLET_ARGS+=(--gcp)
    ;;
  *)
    gl_die "unsupported DEPLOYER_SIGNER=${DEPLOYER_SIGNER}; expected account, keystore, ledger, trezor, aws, gcp, or private-key"
    ;;
  esac

  password_file="${DEPLOYER_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
  if [ -n "${password_file}" ]; then
    [ -f "${password_file}" ] || gl_die "deployer password file does not exist: ${password_file}"
    DEPLOYER_FORGE_WALLET_ARGS+=(--password-file "${password_file}")
    DEPLOYER_CAST_WALLET_ARGS+=(--password-file "${password_file}")
  fi
}

unset DEPLOYER_PRIVATE_KEY
prepare_deployer_wallet_args
export DEPLOYER_ADDRESS="$(cast wallet address "${DEPLOYER_CAST_WALLET_ARGS[@]}")"

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

if [ "$(gl_to_lower "${L1_NETWORK:-}")" = "mainnet" ]; then
  echo "gateway-launch: zkSYS is canonical on L2; skipping L1 DeployErc20"
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
          --sender "${DEPLOYER_ADDRESS}" \
          "${DEPLOYER_FORGE_WALLET_ARGS[@]}" \
          --broadcast \
          --slow 2>&1 | tee "${tmp_erc20_log}"
      else
        timeout "${GATEWAY_DEPLOY_ERC20_TIMEOUT}" \
          forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --sender "${DEPLOYER_ADDRESS}" \
          "${DEPLOYER_FORGE_WALLET_ARGS[@]}" \
          --broadcast 2>&1 | tee "${tmp_erc20_log}"
      fi
      erc20_ec="${PIPESTATUS[0]}"
    else
      if [ "${L1_NETWORK:-}" = "tanenbaum" ] || [ "${L1_NETWORK:-}" = "mainnet" ]; then
        forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --sender "${DEPLOYER_ADDRESS}" \
          "${DEPLOYER_FORGE_WALLET_ARGS[@]}" \
          --broadcast \
          --slow 2>&1 | tee "${tmp_erc20_log}"
      else
        forge script deploy-scripts/tokens/DeployErc20.s.sol \
          --legacy \
          --ffi \
          --rpc-url "${L1_RPC_URL}" \
          --sender "${DEPLOYER_ADDRESS}" \
          "${DEPLOYER_FORGE_WALLET_ARGS[@]}" \
          --broadcast 2>&1 | tee "${tmp_erc20_log}"
      fi
      erc20_ec="${PIPESTATUS[0]}"
    fi
    set -e

    DEV_ZKSYS_TOKEN_ADDRESS="$(extract_zksys_address_from_output || true)"
    if [ "${erc20_ec}" -eq 0 ]; then
      rm -f "${tmp_erc20_log}"
      break
    fi

    if [ -n "${DEV_ZKSYS_TOKEN_ADDRESS}" ] && address_has_code_or_die "${DEV_ZKSYS_TOKEN_ADDRESS}"; then
      echo "gateway-launch: DeployErc20 exited non-zero (${erc20_ec}) but token is deployed at ${DEV_ZKSYS_TOKEN_ADDRESS}; continuing"
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

cd "${GATEWAY_DIR}"

run_ecosystem_init_once() {
  gl_zkstack_pty zkstack ecosystem init \
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
      "${DEPLOYER_FORGE_WALLET_ARGS[@]}" \
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

ecosystem_already_ready=false
if ecosystem_contracts_ready; then
  # SYSCOIN: checkpoint repair/reruns can reach this step after L1 ecosystem
  # contracts were already deployed. Treat confirmed on-chain readiness as
  # idempotent success instead of rerunning one-time initialization.
  echo "gateway-launch: ecosystem contracts already present in configs/contracts.yaml and on-chain; skipping ecosystem init"
  ecosystem_already_ready=true
fi

set_retry_gas_price() {
  local attempt base_wei bump_pct bump_factor gas_price_wei
  attempt="$(normalize_uint "retry attempt" "${1}" "${GATEWAY_ECOSYSTEM_INIT_MAX_ATTEMPTS}")"
  bump_pct="${GATEWAY_RETRY_GAS_BUMP_PCT}"
  base_wei="$(cast gas-price --rpc-url "${L1_RPC_URL}")"
  base_wei="$(normalize_uint "cast gas-price" "${base_wei}" 1000000000000000)"
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

if [ "${ecosystem_already_ready}" != true ]; then
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
fi

deploy_zksys_l1_registry_bridge
