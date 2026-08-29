#!/usr/bin/env bash
# Deploy canonical L2 zkSYS contracts with CREATE2 and wire issuer/registry roles.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_export_foundry_evm_version

gl_require ZKSYS_L2_RPC_URL
gl_require ZKSYS_L2_TOKEN_ADMIN_ADDRESS
gl_require ZKSYS_ISSUER_START_TIME
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${EDGE_CHAIN_NAME:=zksys}"
export GATEWAY_DIR EDGE_CHAIN_NAME L1_CHAIN_ID L1_NETWORK
gl_require L1_NETWORK
gl_require L1_CHAIN_ID
gl_validate_l1_network_pair
gl_assert_edge_chain_config_matches_expected
# SYSCOIN: CREATE2 and role wiring must target the selected edge, never a
# same-looking Gateway or sibling RPC supplied by mistake.
gl_assert_rpc_chain_id_matches_config \
  "${ZKSYS_L2_RPC_URL}" "${EDGE_CHAIN_NAME}" "edge"
: "${ZKSYNC_ERA_PATH:=$(cd "${ZKSYNC_OS_SERVER_PATH}/.." && pwd)/zksync-era}"
# SYSCOIN: This helper compiles and grants roles to privileged L2 contracts.
# Bind its caller-selected Era workspace and every deterministic deployment
# input to the same reviewed V32 launch before deriving any init code.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION ZKSYNC_ERA_PATH
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_assert_contracts_sha
gl_ensure_era_contracts_syscoin_postimage
gl_normalize_canonical_deployment_inputs
gl_require L1_RPC_URL
gl_l1_broadcast_preflight
gl_bind_gateway_launch_context

# SYSCOIN: exact Arachnid deterministic-deployment-proxy attestation. Merely
# finding code at this address is insufficient on a custom-genesis chain.
ARACHNID_CREATE2_DEPLOYER=0x4e59b44847b379578588920cA78FbF26c0B4956C
ARACHNID_CREATE2_RUNTIME=0x7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf3
ARACHNID_CREATE2_RUNTIME_HASH=0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989
: "${ZKSYS_L2_CREATE2_DEPLOYER:=${ARACHNID_CREATE2_DEPLOYER}}"
: "${ZKSYS_L2_TOKEN_NAME:=ZKSYS}"
: "${ZKSYS_L2_TOKEN_SYMBOL:=ZKSYS}"
: "${ZKSYS_L2_TOKEN_DECIMALS:=18}"
: "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS:=0x0000000000000000000000000000000000000000}"
: "${ZKSYS_ISSUER_PERIOD_SECONDS:=86400}"
: "${ZKSYS_ISSUER_PERIODS_PER_YEAR:=365}"
: "${ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS:=3}"
ZERO_ADDRESS="0x0000000000000000000000000000000000000000"

normalize_address_env() {
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

normalize_nonzero_address_env() {
  local name="${1:?name required}"
  local value
  value="$(normalize_address_env "${name}")"
  [ "${value}" != "0x0000000000000000000000000000000000000000" ] || gl_die "${name} must not be zero"
  printf '%s\n' "${value}"
}

load_l1_registry_bridge_address_from_gateway_config() {
  local contracts_yaml="${GATEWAY_DIR:-${HOME}/gateway}/configs/contracts.yaml"
  [ -f "${contracts_yaml}" ] || return 0
  python3 - "${contracts_yaml}" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
# SYSCOIN: Root contracts contain huge decimal bytecode scalars; this reader
# consumes only the persisted registry bridge address.
data = yaml.load(path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader) or {}
addr = data.get("zksys", {}).get("l1_registry_bridge_addr", "")
if isinstance(addr, str) and addr.startswith(("0x", "0X")) and len(addr) == 42:
    print("0x" + format(int(addr[2:], 16), "040x"))
PY
}

normalize_bytes32_env() {
  local name="${1:?name required}"
  local default_value="${2:?default required}"
  python3 - "${name}" "${default_value}" <<'PY'
import os, sys

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

rpc_code() {
  cast code --rpc-url "${ZKSYS_L2_RPC_URL}" "${1:?address required}"
}

assert_exact_runtime() {
  local label="${1:?label required}"
  local address="${2:?address required}"
  local expected_runtime="${3:?expected runtime required}"
  local expected_runtime_hash="${4:?expected runtime hash required}"
  local actual_runtime actual_runtime_hash

  actual_runtime="$(rpc_code "${address}")"
  if [ "$(gl_to_lower "${actual_runtime}")" != "$(gl_to_lower "${expected_runtime}")" ]; then
    gl_die "${label} runtime at ${address} does not match the exact canonical bytecode"
  fi
  actual_runtime_hash="$(cast keccak "${actual_runtime}")"
  if [ "$(gl_to_lower "${actual_runtime_hash}")" != "$(gl_to_lower "${expected_runtime_hash}")" ]; then
    gl_die "${label} runtime hash ${actual_runtime_hash} does not match ${expected_runtime_hash}"
  fi
}

require_create2_deployer() {
  if [ "$(gl_to_lower "${ZKSYS_L2_CREATE2_DEPLOYER}")" != "$(gl_to_lower "${ARACHNID_CREATE2_DEPLOYER}")" ]; then
    gl_die "ZKSYS_L2_CREATE2_DEPLOYER=${ZKSYS_L2_CREATE2_DEPLOYER} is not the canonical Arachnid factory ${ARACHNID_CREATE2_DEPLOYER}"
  fi
  assert_exact_runtime \
    "Arachnid CREATE2 deployer" \
    "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    "${ARACHNID_CREATE2_RUNTIME}" \
    "${ARACHNID_CREATE2_RUNTIME_HASH}"
}

deploy_create2() {
  local label="${1:?label required}"
  local expected_address="${2:?expected address required}"
  local salt="${3:?salt required}"
  local init_code="${4:?init code required}"
  local code

  code="$(rpc_code "${expected_address}")"
  if [ "${code}" != "0x" ]; then
    echo "zksys-l2-bootstrap: ${label} already deployed at ${expected_address}"
    return
  fi

  echo "zksys-l2-bootstrap: deploying ${label} to ${expected_address}"
  cast send \
    --rpc-url "${ZKSYS_L2_RPC_URL}" \
    "${ZKSYS_L2_CAST_WALLET_ARGS[@]}" \
    "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    "${salt}${init_code#0x}" >/dev/null

  code="$(rpc_code "${expected_address}")"
  [ "${code}" != "0x" ] || gl_die "${label} deployment did not create code at ${expected_address}"
}

send_l2() {
  cast send \
    --rpc-url "${ZKSYS_L2_RPC_URL}" \
    "${ZKSYS_L2_CAST_WALLET_ARGS[@]}" \
    "$@" >/dev/null
}

call_l2() {
  cast call \
    --rpc-url "${ZKSYS_L2_RPC_URL}" \
    "$@"
}

assert_l2_address_call() {
  local target="${1:?target required}" signature="${2:?signature required}" expected="${3:?expected required}" actual
  actual="$(call_l2 "${target}" "${signature}")"
  [ "$(gl_to_lower "${actual}")" = "$(gl_to_lower "${expected}")" ] ||
    gl_die "${target} ${signature} returned ${actual}, expected ${expected}"
}

assert_l2_bool_call() {
  local target="${1:?target required}" signature="${2:?signature required}" expected="${3:?expected required}" actual
  shift 3
  actual="$(call_l2 "${target}" "${signature}" "$@")"
  [ "${actual}" = "${expected}" ] || gl_die "${target} ${signature} returned ${actual}, expected ${expected}"
}

assert_l2_uint_call() {
  local target="${1:?target required}" signature="${2:?signature required}" expected="${3:?expected required}" actual
  actual="$(call_l2 "${target}" "${signature}")"
  [ "${actual}" = "${expected}" ] || gl_die "${target} ${signature} returned ${actual}, expected ${expected}"
}

assert_proxy_admin_owner() {
  local proxy_admin="${1:?proxy admin required}" expected_owner="${2:?expected owner required}" actual_owner
  actual_owner="$(call_l2 "${proxy_admin}" "owner()(address)")"
  [ "$(gl_to_lower "${actual_owner}")" = "$(gl_to_lower "${expected_owner}")" ] ||
    gl_die "${proxy_admin} owner()(address) returned ${actual_owner}, expected ${expected_owner}"
}

assert_proxy_wiring() {
  local label="${1:?label required}"
  local proxy_admin="${2:?proxy admin required}"
  local proxy="${3:?proxy required}"
  local expected_implementation="${4:?expected implementation required}"
  local actual_admin actual_implementation

  actual_admin="$(call_l2 "${proxy_admin}" "getProxyAdmin(address)(address)" "${proxy}")"
  [ "$(gl_to_lower "${actual_admin}")" = "$(gl_to_lower "${proxy_admin}")" ] ||
    gl_die "${label} proxy admin mismatch: ${actual_admin} != ${proxy_admin}"

  actual_implementation="$(call_l2 "${proxy_admin}" "getProxyImplementation(address)(address)" "${proxy}")"
  [ "$(gl_to_lower "${actual_implementation}")" = "$(gl_to_lower "${expected_implementation}")" ] ||
    gl_die "${label} proxy implementation mismatch: ${actual_implementation} != ${expected_implementation}"
}

zksys_bootstrap_forge_inspect_dir="$(gl_create_forge_inspect_artifacts_dir)" || exit $?
readonly zksys_bootstrap_forge_inspect_dir
cleanup_zksys_bootstrap_forge_inspect() {
  local rc=$? cleanup_rc=0
  trap - EXIT HUP INT TERM
  gl_remove_forge_inspect_artifacts_dir "${zksys_bootstrap_forge_inspect_dir}" || cleanup_rc=$?
  [ "${rc}" -eq 0 ] || exit "${rc}"
  exit "${cleanup_rc}"
}
trap cleanup_zksys_bootstrap_forge_inspect EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
forge_inspect_bytecode() {
  local contract="${1:?contract required}"
  local inspect_artifacts_dir="${zksys_bootstrap_forge_inspect_dir}"
  # SYSCOIN: production launchers mount reviewed server source read-only. Keep
  # fresh Forge artifacts in owner-private launch state, never in source.
  forge inspect "${contract}" bytecode \
    --no-metadata \
    --root "${inspect_dir}" \
    --out "${inspect_artifacts_dir}/out" \
    --cache-path "${inspect_artifacts_dir}/cache" \
    -R "@openzeppelin/contracts/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/openzeppelin-contracts/contracts/" \
    -R "@openzeppelin/contracts-v4/=${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-v4/contracts/" \
    -R "@openzeppelin/contracts-upgradeable-v4/=${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts/" \
    -R "@openzeppelin/community-contracts/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/openzeppelin-community-contracts/contracts/" \
    -R "forge-std/=${ZKSYNC_OS_SERVER_PATH}/integration-tests/test-contracts/lib/forge-std/src/"
}

prepare_zksys_l2_wallet_args() {
  local signer account_name keystore_path password_file

  if [ -z "${ZKSYS_L2_DEPLOYER_SIGNER:-}" ]; then
    if [ -n "${ZKSYS_L2_DEPLOYER_PRIVATE_KEY:-}" ]; then
      ZKSYS_L2_DEPLOYER_SIGNER="private-key"
    else
      ZKSYS_L2_DEPLOYER_SIGNER="${DEPLOYER_SIGNER:-${FUNDER_SIGNER:-account}}"
    fi
  fi

  signer="$(gl_to_lower "${ZKSYS_L2_DEPLOYER_SIGNER}")"
  ZKSYS_L2_CAST_WALLET_ARGS=()

  case "${signer}" in
  private-key)
    if [ "$(gl_to_lower "${L1_NETWORK:-}")" = "mainnet" ] || [ "$(gl_to_lower "${L1_NETWORK:-}")" = "tanenbaum" ]; then
      if ! gl_allow_insecure_private_key_argv; then
        gl_die "ZKSYS_L2_DEPLOYER_SIGNER=private-key is not allowed on ${L1_NETWORK}; use account, keystore, hardware wallet, or KMS signing"
      fi
    fi
    gl_require ZKSYS_L2_DEPLOYER_PRIVATE_KEY
    ZKSYS_L2_CAST_WALLET_ARGS+=(--private-key "${ZKSYS_L2_DEPLOYER_PRIVATE_KEY}")
    ;;
  account)
    account_name="${ZKSYS_L2_DEPLOYER_ACCOUNT_NAME:-${DEPLOYER_ACCOUNT_NAME:-${FUNDER_ACCOUNT_NAME:-funder}}}"
    [ -n "${account_name}" ] || gl_die "ZKSYS_L2_DEPLOYER_ACCOUNT_NAME must not be empty"
    gl_validate_foundry_account_keystore \
      "${account_name}" "ZKSYS_L2_DEPLOYER_ACCOUNT_NAME"
    ZKSYS_L2_CAST_WALLET_ARGS+=(--account "${account_name}")
    ;;
  keystore)
    keystore_path="${ZKSYS_L2_DEPLOYER_KEYSTORE:-${DEPLOYER_KEYSTORE:-${FUNDER_KEYSTORE:-}}}"
    [ -n "${keystore_path}" ] || gl_die "ZKSYS_L2_DEPLOYER_KEYSTORE is required when ZKSYS_L2_DEPLOYER_SIGNER=keystore"
    gl_validate_secret_file "${keystore_path}" "ZKSYS_L2_DEPLOYER_KEYSTORE"
    ZKSYS_L2_CAST_WALLET_ARGS+=(--keystore "${keystore_path}")
    ;;
  ledger)
    ZKSYS_L2_CAST_WALLET_ARGS+=(--ledger)
    ;;
  trezor)
    ZKSYS_L2_CAST_WALLET_ARGS+=(--trezor)
    ;;
  aws)
    ZKSYS_L2_CAST_WALLET_ARGS+=(--aws)
    ;;
  gcp)
    ZKSYS_L2_CAST_WALLET_ARGS+=(--gcp)
    ;;
  *)
    gl_die "unsupported ZKSYS_L2_DEPLOYER_SIGNER=${ZKSYS_L2_DEPLOYER_SIGNER}; expected account, keystore, ledger, trezor, aws, gcp, or private-key"
    ;;
  esac

  password_file="${ZKSYS_L2_DEPLOYER_PASSWORD_FILE:-${DEPLOYER_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}}"
  if [ -n "${password_file}" ]; then
    gl_validate_secret_file "${password_file}" "ZKSYS_L2_DEPLOYER_PASSWORD_FILE"
    ZKSYS_L2_CAST_WALLET_ARGS+=(--password-file "${password_file}")
  fi
}

ZKSYS_L2_CREATE2_DEPLOYER="$(normalize_nonzero_address_env ZKSYS_L2_CREATE2_DEPLOYER)"
ZKSYS_L2_TOKEN_ADMIN_ADDRESS="$(normalize_nonzero_address_env ZKSYS_L2_TOKEN_ADMIN_ADDRESS)"
configured_l1_registry_bridge="$(load_l1_registry_bridge_address_from_gateway_config || true)"
if [ "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" = "${ZERO_ADDRESS}" ] &&
  [ -n "${configured_l1_registry_bridge}" ]; then
  ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS="${configured_l1_registry_bridge}"
fi
ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS="$(normalize_address_env ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS)"
if [ "${L1_NETWORK}" = tanenbaum ] || [ "${L1_NETWORK}" = mainnet ]; then
  [ -n "${configured_l1_registry_bridge}" ] &&
    [ "${configured_l1_registry_bridge}" != "${ZERO_ADDRESS}" ] ||
    gl_die "canonical L2 bootstrap requires persisted zksys.l1_registry_bridge_addr"
  [ "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" = "${configured_l1_registry_bridge}" ] ||
    gl_die "ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS must equal persisted zksys.l1_registry_bridge_addr"
fi
export ZKSYS_L2_CREATE2_DEPLOYER
export ZKSYS_L2_TOKEN_ADMIN_ADDRESS
export ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS

case "${ZKSYS_L2_TOKEN_DECIMALS}" in
''|*[!0-9]*) gl_die "ZKSYS_L2_TOKEN_DECIMALS must be a uint8" ;;
esac
[ "${ZKSYS_L2_TOKEN_DECIMALS}" -le 59 ] || gl_die "ZKSYS_L2_TOKEN_DECIMALS must be <= 59"
for schedule_var in ZKSYS_ISSUER_START_TIME ZKSYS_ISSUER_PERIOD_SECONDS ZKSYS_ISSUER_PERIODS_PER_YEAR ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS; do
  case "${!schedule_var}" in
  ''|*[!0-9]*) gl_die "${schedule_var} must be a decimal uint256" ;;
  esac
done
[ "${ZKSYS_ISSUER_PERIOD_SECONDS}" != "0" ] || gl_die "ZKSYS_ISSUER_PERIOD_SECONDS must be non-zero"
[ "${ZKSYS_ISSUER_PERIODS_PER_YEAR}" != "0" ] || gl_die "ZKSYS_ISSUER_PERIODS_PER_YEAR must be non-zero"
[ "${ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS}" != "0" ] || gl_die "ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS must be non-zero"
[ "${ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS}" -le 7 ] || gl_die "ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS must be <= 7"
python3 - "${ZKSYS_ISSUER_PERIOD_SECONDS}" "${ZKSYS_ISSUER_PERIODS_PER_YEAR}" <<'PY'
import sys

period_seconds = int(sys.argv[1])
periods_per_year = int(sys.argv[2])
if period_seconds * periods_per_year != 365 * 24 * 60 * 60:
    raise SystemExit("ZKSYS_ISSUER_PERIOD_SECONDS * ZKSYS_ISSUER_PERIODS_PER_YEAR must equal 365 days")
PY

prepare_zksys_l2_wallet_args
BOOTSTRAP_SIGNER_ADDRESS="$(cast wallet address "${ZKSYS_L2_CAST_WALLET_ARGS[@]}")"
if [ "$(gl_to_lower "${BOOTSTRAP_SIGNER_ADDRESS}")" != "$(gl_to_lower "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}")" ]; then
  gl_die "ZKSYS_L2_DEPLOYER_SIGNER must control ZKSYS_L2_TOKEN_ADMIN_ADDRESS for role wiring"
fi

ZKSYS_L2_PROXY_ADMIN_SALT="$(normalize_bytes32_env ZKSYS_L2_PROXY_ADMIN_SALT 0x7a6b7379732d70726f78792d61646d696e000000000000000000000000000000)"
ZKSYS_L2_TOKEN_IMPL_SALT="$(normalize_bytes32_env ZKSYS_L2_TOKEN_IMPL_SALT 0x7a6b7379732d746f6b656e2d696d706c00000000000000000000000000000000)"
ZKSYS_L2_TOKEN_PROXY_SALT="$(normalize_bytes32_env ZKSYS_L2_TOKEN_PROXY_SALT 0x7a6b7379732d746f6b656e2d70726f7879000000000000000000000000000000)"
ZKSYS_L2_REGISTRY_IMPL_SALT="$(normalize_bytes32_env ZKSYS_L2_REGISTRY_IMPL_SALT 0x7a6b7379732d72656769737472792d696d706c00000000000000000000000000)"
ZKSYS_L2_REGISTRY_PROXY_SALT="$(normalize_bytes32_env ZKSYS_L2_REGISTRY_PROXY_SALT 0x7a6b7379732d72656769737472792d70726f7879000000000000000000000000)"
ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT="$(normalize_bytes32_env ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT 0x7a6b7379732d7765696768742d72656769737472792d696d706c000000000000)"
ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT="$(normalize_bytes32_env ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT 0x7a6b7379732d7765696768742d72656769737472792d70726f78790000000000)"
ZKSYS_L2_ISSUER_IMPL_SALT="$(normalize_bytes32_env ZKSYS_L2_ISSUER_IMPL_SALT 0x7a6b7379732d6973737565722d696d706c000000000000000000000000000000)"
ZKSYS_L2_ISSUER_PROXY_SALT="$(normalize_bytes32_env ZKSYS_L2_ISSUER_PROXY_SALT 0x7a6b7379732d6973737565722d70726f78790000000000000000000000000000)"
ZKSYS_L2_STAKING_VAULT_IMPL_SALT="$(normalize_bytes32_env ZKSYS_L2_STAKING_VAULT_IMPL_SALT 0x7a6b7379732d7374616b696e672d7661756c742d696d706c0000000000000000)"
ZKSYS_L2_STAKING_VAULT_PROXY_SALT="$(normalize_bytes32_env ZKSYS_L2_STAKING_VAULT_PROXY_SALT 0x7a6b7379732d7374616b696e672d7661756c742d70726f787900000000000000)"
ZKSYS_L2_GAS_TANK_SALT="$(normalize_bytes32_env ZKSYS_L2_GAS_TANK_SALT 0x7a6b7379732d6761732d74616e6b000000000000000000000000000000000000)"
export ZKSYS_L2_PROXY_ADMIN_SALT ZKSYS_L2_TOKEN_IMPL_SALT ZKSYS_L2_TOKEN_PROXY_SALT
export ZKSYS_L2_REGISTRY_IMPL_SALT ZKSYS_L2_REGISTRY_PROXY_SALT
export ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT
export ZKSYS_L2_ISSUER_IMPL_SALT ZKSYS_L2_ISSUER_PROXY_SALT
export ZKSYS_L2_STAKING_VAULT_IMPL_SALT ZKSYS_L2_STAKING_VAULT_PROXY_SALT
export ZKSYS_L2_GAS_TANK_SALT ZKSYS_L2_TOKEN_NAME ZKSYS_L2_TOKEN_SYMBOL
export ZKSYS_L2_TOKEN_DECIMALS ZKSYS_ISSUER_START_TIME ZKSYS_ISSUER_PERIOD_SECONDS
export ZKSYS_ISSUER_PERIODS_PER_YEAR ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS

bind_zksys_l2_bootstrap_manifest() {
  local manifest_path
  manifest_path="$(gl_checkpoint_state_dir)/zksys-l2-bootstrap.json"
  python3 - \
    "${manifest_path}" "${GL_DIR}" \
    "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/ZkStack.yaml" <<'PY'
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path

import yaml

manifest_path = Path(sys.argv[1])
sys.path.insert(0, sys.argv[2])
from _checkpoint_state_io import atomic_write_json

chain_path = Path(sys.argv[3])
chain = yaml.safe_load(chain_path.read_text(encoding="utf-8")) or {}
chain_id = chain.get("chain_id")
if isinstance(chain_id, str):
    chain_id = int(chain_id, 0)
if not isinstance(chain_id, int) or isinstance(chain_id, bool) or chain_id <= 0:
    raise SystemExit(f"invalid edge chain id in {chain_path}")

names = (
    "ZKSYS_L2_PROXY_ADMIN_SALT",
    "ZKSYS_L2_TOKEN_IMPL_SALT",
    "ZKSYS_L2_TOKEN_PROXY_SALT",
    "ZKSYS_L2_REGISTRY_IMPL_SALT",
    "ZKSYS_L2_REGISTRY_PROXY_SALT",
    "ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT",
    "ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT",
    "ZKSYS_L2_ISSUER_IMPL_SALT",
    "ZKSYS_L2_ISSUER_PROXY_SALT",
    "ZKSYS_L2_STAKING_VAULT_IMPL_SALT",
    "ZKSYS_L2_STAKING_VAULT_PROXY_SALT",
    "ZKSYS_L2_GAS_TANK_SALT",
    "ZKSYS_L2_TOKEN_NAME",
    "ZKSYS_L2_TOKEN_SYMBOL",
    "ZKSYS_L2_TOKEN_DECIMALS",
    "ZKSYS_ISSUER_START_TIME",
    "ZKSYS_ISSUER_PERIOD_SECONDS",
    "ZKSYS_ISSUER_PERIODS_PER_YEAR",
    "ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS",
)
derived_names = (
    "ZKSYS_L2_PROXY_ADMIN_ADDRESS",
    "ZKSYS_L2_TOKEN_IMPL_ADDRESS",
    "ZKSYS_L2_TOKEN_ADDRESS",
    "ZKSYS_L2_REGISTRY_IMPL_ADDRESS",
    "ZKSYS_L2_REGISTRY_ADDRESS",
    "ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS",
    "ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS",
    "ZKSYS_L2_ISSUER_IMPL_ADDRESS",
    "ZKSYS_L2_ISSUER_ADDRESS",
    "ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS",
    "ZKSYS_L2_STAKING_VAULT_ADDRESS",
    "ZKSYS_L2_GAS_TANK_ADDRESS",
)
init_hash_names = (
    "ZKSYS_L2_PROXY_ADMIN_INIT_CODE_HASH",
    "ZKSYS_L2_TOKEN_IMPL_INIT_CODE_HASH",
    "ZKSYS_L2_TOKEN_PROXY_INIT_CODE_HASH",
    "ZKSYS_L2_REGISTRY_IMPL_INIT_CODE_HASH",
    "ZKSYS_L2_REGISTRY_PROXY_INIT_CODE_HASH",
    "ZKSYS_L2_WEIGHT_REGISTRY_IMPL_INIT_CODE_HASH",
    "ZKSYS_L2_WEIGHT_REGISTRY_PROXY_INIT_CODE_HASH",
    "ZKSYS_L2_ISSUER_IMPL_INIT_CODE_HASH",
    "ZKSYS_L2_ISSUER_PROXY_INIT_CODE_HASH",
    "ZKSYS_L2_STAKING_VAULT_IMPL_INIT_CODE_HASH",
    "ZKSYS_L2_STAKING_VAULT_PROXY_INIT_CODE_HASH",
    "ZKSYS_L2_GAS_TANK_INIT_CODE_HASH",
)


def normalized_hex(name, nybbles):
    value = os.environ[name].strip().lower()
    if not re.fullmatch(rf"0x[0-9a-f]{{{nybbles}}}", value):
        raise SystemExit(f"invalid derived bootstrap identity {name}")
    return value


payload = {
    "schema_version": 2,
    "protocol_version": os.environ["PROTOCOL_VERSION"],
    "required_zkstack_cli_sha": os.environ["REQUIRED_ZKSTACK_CLI_SHA"],
    "required_contracts_sha": os.environ["REQUIRED_CONTRACTS_SHA"],
    "l1_chain_id": os.environ["L1_CHAIN_ID"],
    "l1_network": os.environ["L1_NETWORK"],
    "edge_chain_name": os.environ.get("EDGE_CHAIN_NAME", "zksys"),
    "edge_chain_id": str(chain_id),
    "l2_rpc_url_sha256": hashlib.sha256(os.environ["ZKSYS_L2_RPC_URL"].encode()).hexdigest(),
    "l2_create2_deployer": os.environ["ZKSYS_L2_CREATE2_DEPLOYER"].lower(),
    "l2_token_admin": os.environ["ZKSYS_L2_TOKEN_ADMIN_ADDRESS"].lower(),
    "l1_registry_bridge": os.environ["ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS"].lower(),
    "inputs": {name: os.environ[name] for name in names},
    "derived_addresses": {name: normalized_hex(name, 40) for name in derived_names},
    "init_code_hashes": {name: normalized_hex(name, 64) for name in init_hash_names},
}

if manifest_path.exists() or manifest_path.is_symlink():
    info = os.lstat(manifest_path)
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise SystemExit(f"unsafe zkSYS bootstrap manifest: {manifest_path}")
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise SystemExit(f"unsafe zkSYS bootstrap manifest ownership/mode: {manifest_path}")
    current = json.loads(manifest_path.read_text(encoding="utf-8"))
    if current != payload:
        changed = sorted(key for key in payload if current.get(key) != payload.get(key))
        raise SystemExit(
            "zkSYS bootstrap identity differs from its first run: " + ", ".join(changed)
        )
else:
    atomic_write_json(manifest_path, payload)
PY
}

inspect_dir="${ZKSYNC_OS_SERVER_PATH}/contracts"
[ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-v4/contracts" ] ||
  gl_die "missing OpenZeppelin v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"
[ -d "${ZKSYNC_ERA_PATH}/contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts" ] ||
  gl_die "missing OpenZeppelin upgradeable v4 contracts under ZKSYNC_ERA_PATH=${ZKSYNC_ERA_PATH}"

proxy_admin_ctor_args="$(cast abi-encode "constructor(address)" "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}")"
proxy_admin_init_code="$(forge_inspect_bytecode ZkSysProxyAdmin)${proxy_admin_ctor_args#0x}"
ZKSYS_L2_PROXY_ADMIN_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_PROXY_ADMIN_SALT}" \
    --init-code "${proxy_admin_init_code}"
)"

token_impl_init_code="$(forge_inspect_bytecode SyscoinZKSYSToken)"
ZKSYS_L2_TOKEN_IMPL_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_TOKEN_IMPL_SALT}" \
    --init-code "${token_impl_init_code}"
)"

token_init_data="$(
  cast calldata \
    "initialize(string,string,uint8,address)" \
    "${ZKSYS_L2_TOKEN_NAME}" \
    "${ZKSYS_L2_TOKEN_SYMBOL}" \
    "${ZKSYS_L2_TOKEN_DECIMALS}" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}"
)"
token_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${ZKSYS_L2_TOKEN_IMPL_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${token_init_data}")"
token_proxy_init_code="$(forge_inspect_bytecode ZkSysCreate2ProxyBytecode)${token_proxy_ctor_args#0x}"
ZKSYS_L2_TOKEN_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_TOKEN_PROXY_SALT}" \
    --init-code "${token_proxy_init_code}"
)"

registry_impl_init_code="$(forge_inspect_bytecode ZkSysMembershipRegistry)"
ZKSYS_L2_REGISTRY_IMPL_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_REGISTRY_IMPL_SALT}" \
    --init-code "${registry_impl_init_code}"
)"
registry_init_data="$(
  cast calldata \
    "initialize(address,address)" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZERO_ADDRESS}"
)"
registry_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${ZKSYS_L2_REGISTRY_IMPL_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${registry_init_data}")"
registry_proxy_init_code="$(forge_inspect_bytecode ZkSysCreate2ProxyBytecode)${registry_proxy_ctor_args#0x}"
ZKSYS_L2_REGISTRY_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_REGISTRY_PROXY_SALT}" \
    --init-code "${registry_proxy_init_code}"
)"

if [ "${L1_NETWORK}" = tanenbaum ] || [ "${L1_NETWORK}" = mainnet ]; then
  # SYSCOIN: setL1RegistryBridge is one-shot. Reuse the full deterministic L1
  # deployment attestor without its signer or mutation paths before any L2 send.
  "${SCRIPT_DIR}/zksys-l1-registry-bridge-only.sh" --check-only
fi

weight_registry_impl_init_code="$(forge_inspect_bytecode ZkSysRewardWeightRegistry)"
ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT}" \
    --init-code "${weight_registry_impl_init_code}"
)"
weight_registry_init_data="$(
  cast calldata \
    "initialize(address,address,uint256)" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZKSYS_L2_REGISTRY_ADDRESS}" \
    "${ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS}"
)"
weight_registry_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${weight_registry_init_data}")"
weight_registry_proxy_init_code="$(forge_inspect_bytecode ZkSysCreate2ProxyBytecode)${weight_registry_proxy_ctor_args#0x}"
ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT}" \
    --init-code "${weight_registry_proxy_init_code}"
)"

issuer_impl_init_code="$(forge_inspect_bytecode ZkSysIssuer)"
ZKSYS_L2_ISSUER_IMPL_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_ISSUER_IMPL_SALT}" \
    --init-code "${issuer_impl_init_code}"
)"
issuer_init_data="$(
  cast calldata \
    "initialize(address,address,address,uint256,uint256,uint256)" \
    "${ZKSYS_L2_TOKEN_ADDRESS}" \
    "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZKSYS_ISSUER_START_TIME}" \
    "${ZKSYS_ISSUER_PERIOD_SECONDS}" \
    "${ZKSYS_ISSUER_PERIODS_PER_YEAR}"
)"
issuer_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${ZKSYS_L2_ISSUER_IMPL_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${issuer_init_data}")"
issuer_proxy_init_code="$(forge_inspect_bytecode ZkSysCreate2ProxyBytecode)${issuer_proxy_ctor_args#0x}"
ZKSYS_L2_ISSUER_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_ISSUER_PROXY_SALT}" \
    --init-code "${issuer_proxy_init_code}"
)"

staking_vault_impl_init_code="$(forge_inspect_bytecode ZkSysNativeStakingVault)"
ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_STAKING_VAULT_IMPL_SALT}" \
    --init-code "${staking_vault_impl_init_code}"
)"
staking_vault_init_data="$(
  cast calldata \
    "initialize(address)" \
    "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}"
)"
staking_vault_proxy_ctor_args="$(cast abi-encode "constructor(address,address,bytes)" "${ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${staking_vault_init_data}")"
staking_vault_proxy_init_code="$(forge_inspect_bytecode ZkSysCreate2ProxyBytecode)${staking_vault_proxy_ctor_args#0x}"
ZKSYS_L2_STAKING_VAULT_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_STAKING_VAULT_PROXY_SALT}" \
    --init-code "${staking_vault_proxy_init_code}"
)"

# SYSCOIN: prepaid zkSYS gas ledger debited by the patched ZKsync OS
# bootloader. Non-upgradeable and atomic by construction: the constructor
# pins the token; the only wiring is the BURNER_ROLE grant for burnSurplus().
gas_tank_ctor_args="$(cast abi-encode "constructor(address)" "${ZKSYS_L2_TOKEN_ADDRESS}")"
gas_tank_creation_code="$(forge_inspect_bytecode ZkSysGasTank)"
gas_tank_init_code="${gas_tank_creation_code}${gas_tank_ctor_args#0x}"
gas_tank_init_code_hash="$(cast keccak "${gas_tank_init_code}")"
ZKSYS_L2_GAS_TANK_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_GAS_TANK_SALT}" \
    --init-code "${gas_tank_init_code}"
)"
PUBLISHED_GAS_TANK_INIT_CODE_HASH=0x1fce42acba699bc198d2e146b0284e3bdd821d1634cd809f1c0a12e961dac561
PUBLISHED_GAS_TANK_RUNTIME_HASH=0x041faf31b2f3576502f25fd5d106eaf411611e42dc996c28872abe487cb6e269
PUBLISHED_GAS_TANK_ADDRESS=0xb49943ea232624dd4aa63e18186076c6c99a68ef
[ "$(gl_to_lower "${gas_tank_init_code_hash}")" = "${PUBLISHED_GAS_TANK_INIT_CODE_HASH}" ] || \
  gl_die "derived gas tank init-code hash ${gas_tank_init_code_hash} differs from the canonical value ${PUBLISHED_GAS_TANK_INIT_CODE_HASH}; changing it requires a new app, VK, and verifier"
[ "$(printf '%s' "${ZKSYS_L2_GAS_TANK_ADDRESS}" | tr '[:upper:]' '[:lower:]')" = \
  "${PUBLISHED_GAS_TANK_ADDRESS}" ] || \
  gl_die "derived gas tank ${ZKSYS_L2_GAS_TANK_ADDRESS} differs from the canonical app value ${PUBLISHED_GAS_TANK_ADDRESS}; changing it requires a new app, VK, and verifier"

# SYSCOIN: Bind both the normalized inputs and their complete derived CREATE2
# graph before the first deployment. A retry after any source/tooling change
# therefore fails closed instead of creating a second privileged contract set.
ZKSYS_L2_PROXY_ADMIN_INIT_CODE_HASH="$(cast keccak "${proxy_admin_init_code}")"
ZKSYS_L2_TOKEN_IMPL_INIT_CODE_HASH="$(cast keccak "${token_impl_init_code}")"
ZKSYS_L2_TOKEN_PROXY_INIT_CODE_HASH="$(cast keccak "${token_proxy_init_code}")"
ZKSYS_L2_REGISTRY_IMPL_INIT_CODE_HASH="$(cast keccak "${registry_impl_init_code}")"
ZKSYS_L2_REGISTRY_PROXY_INIT_CODE_HASH="$(cast keccak "${registry_proxy_init_code}")"
ZKSYS_L2_WEIGHT_REGISTRY_IMPL_INIT_CODE_HASH="$(cast keccak "${weight_registry_impl_init_code}")"
ZKSYS_L2_WEIGHT_REGISTRY_PROXY_INIT_CODE_HASH="$(cast keccak "${weight_registry_proxy_init_code}")"
ZKSYS_L2_ISSUER_IMPL_INIT_CODE_HASH="$(cast keccak "${issuer_impl_init_code}")"
ZKSYS_L2_ISSUER_PROXY_INIT_CODE_HASH="$(cast keccak "${issuer_proxy_init_code}")"
ZKSYS_L2_STAKING_VAULT_IMPL_INIT_CODE_HASH="$(cast keccak "${staking_vault_impl_init_code}")"
ZKSYS_L2_STAKING_VAULT_PROXY_INIT_CODE_HASH="$(cast keccak "${staking_vault_proxy_init_code}")"
ZKSYS_L2_GAS_TANK_INIT_CODE_HASH="${gas_tank_init_code_hash}"
export ZKSYS_L2_PROXY_ADMIN_ADDRESS ZKSYS_L2_TOKEN_IMPL_ADDRESS ZKSYS_L2_TOKEN_ADDRESS
export ZKSYS_L2_REGISTRY_IMPL_ADDRESS ZKSYS_L2_REGISTRY_ADDRESS
export ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS
export ZKSYS_L2_ISSUER_IMPL_ADDRESS ZKSYS_L2_ISSUER_ADDRESS
export ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS ZKSYS_L2_STAKING_VAULT_ADDRESS
export ZKSYS_L2_GAS_TANK_ADDRESS
export ZKSYS_L2_PROXY_ADMIN_INIT_CODE_HASH ZKSYS_L2_TOKEN_IMPL_INIT_CODE_HASH
export ZKSYS_L2_TOKEN_PROXY_INIT_CODE_HASH ZKSYS_L2_REGISTRY_IMPL_INIT_CODE_HASH
export ZKSYS_L2_REGISTRY_PROXY_INIT_CODE_HASH ZKSYS_L2_WEIGHT_REGISTRY_IMPL_INIT_CODE_HASH
export ZKSYS_L2_WEIGHT_REGISTRY_PROXY_INIT_CODE_HASH ZKSYS_L2_ISSUER_IMPL_INIT_CODE_HASH
export ZKSYS_L2_ISSUER_PROXY_INIT_CODE_HASH ZKSYS_L2_STAKING_VAULT_IMPL_INIT_CODE_HASH
export ZKSYS_L2_STAKING_VAULT_PROXY_INIT_CODE_HASH ZKSYS_L2_GAS_TANK_INIT_CODE_HASH
bind_zksys_l2_bootstrap_manifest

require_create2_deployer
deploy_create2 "zkSYS proxy admin" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_SALT}" "${proxy_admin_init_code}"
deploy_create2 "zkSYS token implementation" "${ZKSYS_L2_TOKEN_IMPL_ADDRESS}" "${ZKSYS_L2_TOKEN_IMPL_SALT}" "${token_impl_init_code}"
deploy_create2 "zkSYS token proxy" "${ZKSYS_L2_TOKEN_ADDRESS}" "${ZKSYS_L2_TOKEN_PROXY_SALT}" "${token_proxy_init_code}"
deploy_create2 "zkSYS membership registry implementation" "${ZKSYS_L2_REGISTRY_IMPL_ADDRESS}" "${ZKSYS_L2_REGISTRY_IMPL_SALT}" "${registry_impl_init_code}"
deploy_create2 "zkSYS membership registry proxy" "${ZKSYS_L2_REGISTRY_ADDRESS}" "${ZKSYS_L2_REGISTRY_PROXY_SALT}" "${registry_proxy_init_code}"
deploy_create2 "zkSYS reward weight registry implementation" "${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS}" "${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT}" "${weight_registry_impl_init_code}"
deploy_create2 "zkSYS reward weight registry proxy" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "${ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT}" "${weight_registry_proxy_init_code}"
deploy_create2 "zkSYS issuer implementation" "${ZKSYS_L2_ISSUER_IMPL_ADDRESS}" "${ZKSYS_L2_ISSUER_IMPL_SALT}" "${issuer_impl_init_code}"
deploy_create2 "zkSYS issuer proxy" "${ZKSYS_L2_ISSUER_ADDRESS}" "${ZKSYS_L2_ISSUER_PROXY_SALT}" "${issuer_proxy_init_code}"
deploy_create2 "zkSYS native staking vault implementation" "${ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS}" "${ZKSYS_L2_STAKING_VAULT_IMPL_SALT}" "${staking_vault_impl_init_code}"
deploy_create2 "zkSYS native staking vault proxy" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}" "${ZKSYS_L2_STAKING_VAULT_PROXY_SALT}" "${staking_vault_proxy_init_code}"

# SYSCOIN: execute the constructor against the now-live canonical token proxy
# to obtain the immutable-specialized runtime. Reject a preexisting or newly
# deployed impostor byte-for-byte and by hash before granting any burn power.
expected_gas_tank_runtime="$(
  cast call \
    --rpc-url "${ZKSYS_L2_RPC_URL}" \
    --create "${gas_tank_creation_code}" \
    "constructor(address)" "${ZKSYS_L2_TOKEN_ADDRESS}"
)"
expected_gas_tank_runtime_hash="$(cast keccak "${expected_gas_tank_runtime}")"
[ "$(gl_to_lower "${expected_gas_tank_runtime_hash}")" = "${PUBLISHED_GAS_TANK_RUNTIME_HASH}" ] || \
  gl_die "derived gas tank runtime hash ${expected_gas_tank_runtime_hash} differs from the canonical value ${PUBLISHED_GAS_TANK_RUNTIME_HASH}; changing it requires a new app, VK, and verifier"
deploy_create2 "zkSYS gas tank" "${ZKSYS_L2_GAS_TANK_ADDRESS}" "${ZKSYS_L2_GAS_TANK_SALT}" "${gas_tank_init_code}"
assert_exact_runtime \
  "zkSYS gas tank" \
  "${ZKSYS_L2_GAS_TANK_ADDRESS}" \
  "${expected_gas_tank_runtime}" \
  "${PUBLISHED_GAS_TANK_RUNTIME_HASH}"

echo "zksys-l2-bootstrap: verifying proxy admin and implementation wiring"
assert_proxy_admin_owner "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}"
assert_proxy_wiring "zkSYS token" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_TOKEN_ADDRESS}" "${ZKSYS_L2_TOKEN_IMPL_ADDRESS}"
assert_proxy_wiring "zkSYS membership registry" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_REGISTRY_ADDRESS}" "${ZKSYS_L2_REGISTRY_IMPL_ADDRESS}"
assert_proxy_wiring "zkSYS reward weight registry" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS}"
assert_proxy_wiring "zkSYS issuer" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_ISSUER_ADDRESS}" "${ZKSYS_L2_ISSUER_IMPL_ADDRESS}"
assert_proxy_wiring "zkSYS native staking vault" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}" "${ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS}"

MINTER_ROLE="$(cast keccak "$(cast from-ascii MINTER_ROLE)")"
BURNER_ROLE="$(cast keccak "$(cast from-ascii BURNER_ROLE)")"
STAKE_WEIGHT_UPDATER_ROLE="$(cast keccak "$(cast from-ascii STAKE_WEIGHT_UPDATER_ROLE)")"

echo "zksys-l2-bootstrap: wiring issuer minter role and registry receivers"
send_l2 "${ZKSYS_L2_TOKEN_ADDRESS}" "grantRole(bytes32,address)" "${MINTER_ROLE}" "${ZKSYS_L2_ISSUER_ADDRESS}"
send_l2 "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "grantRole(bytes32,address)" "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}"
send_l2 "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "setWeightReceiver(address)" "${ZKSYS_L2_ISSUER_ADDRESS}"
send_l2 "${ZKSYS_L2_REGISTRY_ADDRESS}" "setSentryNodeReceiver(address)" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}"
if [ "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" != "${ZERO_ADDRESS}" ]; then
  send_l2 "${ZKSYS_L2_REGISTRY_ADDRESS}" "setL1RegistryBridge(address)" "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}"
fi

echo "zksys-l2-bootstrap: wiring gas tank burner role"
send_l2 "${ZKSYS_L2_TOKEN_ADDRESS}" "grantRole(bytes32,address)" "${BURNER_ROLE}" "${ZKSYS_L2_GAS_TANK_ADDRESS}"

echo "zksys-l2-bootstrap: verifying role and receiver wiring"
assert_l2_bool_call "${ZKSYS_L2_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "true" "${MINTER_ROLE}" "${ZKSYS_L2_ISSUER_ADDRESS}"
assert_l2_bool_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "hasRole(bytes32,address)(bool)" "true" "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}"
assert_l2_uint_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "activationDelayPeriods()(uint256)" "${ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS}"
assert_l2_address_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "weightReceiver()(address)" "${ZKSYS_L2_ISSUER_ADDRESS}"
assert_l2_address_call "${ZKSYS_L2_REGISTRY_ADDRESS}" "sentryNodeReceiver()(address)" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}"
assert_l2_address_call "${ZKSYS_L2_STAKING_VAULT_ADDRESS}" "weightRegistry()(address)" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}"
if [ "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" != "${ZERO_ADDRESS}" ]; then
  assert_l2_address_call "${ZKSYS_L2_REGISTRY_ADDRESS}" "l1RegistryBridge()(address)" "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}"
fi
assert_l2_address_call "${ZKSYS_L2_GAS_TANK_ADDRESS}" "token()(address)" "${ZKSYS_L2_TOKEN_ADDRESS}"
assert_l2_bool_call "${ZKSYS_L2_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "true" "${BURNER_ROLE}" "${ZKSYS_L2_GAS_TANK_ADDRESS}"

cat <<EOF
zksys-l2-bootstrap: complete
  proxyAdmin          = ${ZKSYS_L2_PROXY_ADMIN_ADDRESS}
  tokenImplementation = ${ZKSYS_L2_TOKEN_IMPL_ADDRESS}
  tokenProxy          = ${ZKSYS_L2_TOKEN_ADDRESS}
  registryImpl        = ${ZKSYS_L2_REGISTRY_IMPL_ADDRESS}
  registryProxy       = ${ZKSYS_L2_REGISTRY_ADDRESS}
  weightRegistryImpl  = ${ZKSYS_L2_WEIGHT_REGISTRY_IMPL_ADDRESS}
  weightRegistryProxy = ${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}
  issuerImpl          = ${ZKSYS_L2_ISSUER_IMPL_ADDRESS}
  issuerProxy         = ${ZKSYS_L2_ISSUER_ADDRESS}
  stakingVaultImpl    = ${ZKSYS_L2_STAKING_VAULT_IMPL_ADDRESS}
  stakingVaultProxy   = ${ZKSYS_L2_STAKING_VAULT_ADDRESS}
  gasTank             = ${ZKSYS_L2_GAS_TANK_ADDRESS}
EOF

# SYSCOIN: persist the already-attested gas-tank address so launchers can
# validate deployment state against the canonical app.
# Resolve the target file with the same priority as the reader
# (gl_zksys_gas_tank_from_edge_config): canonical contracts.yaml first, then
# the zkstack-emitted contracts_<chain-id>.yaml layout.
zksys_configs_dir="${GATEWAY_DIR:-${HOME}/gateway}/chains/${EDGE_CHAIN_NAME:-zksys}/configs"
zksys_contracts_yaml="${zksys_configs_dir}/contracts.yaml"
if [ ! -f "${zksys_contracts_yaml}" ] && [ -n "${EDGE_CHAIN_ID:-}" ] && [ -f "${zksys_configs_dir}/contracts_${EDGE_CHAIN_ID}.yaml" ]; then
  zksys_contracts_yaml="${zksys_configs_dir}/contracts_${EDGE_CHAIN_ID}.yaml"
fi
if [ ! -f "${zksys_contracts_yaml}" ]; then
  for candidate in "${zksys_configs_dir}"/contracts_*.yaml; do
    [ -f "${candidate}" ] || continue
    zksys_contracts_yaml="${candidate}"
    break
  done
fi
if [ -f "${zksys_contracts_yaml}" ]; then
  python3 - "${zksys_contracts_yaml}" "${ZKSYS_L2_GAS_TANK_ADDRESS}" <<'PY'
import re
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
address = sys.argv[2].strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", address) or address == "0x" + "0" * 40:
    raise SystemExit("gas tank address must be a nonzero 20-byte hex address")
if int(address[2:], 16) < 1 << 16:
    raise SystemExit("gas tank address must not be in the reserved system address space")

data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {path}")
l2 = data.setdefault("l2", {})
if not isinstance(l2, dict):
    raise SystemExit(f"invalid l2 section in {path}")
l2["zksys_gas_tank_addr"] = address
path.write_text(yaml.safe_dump(data, sort_keys=False, allow_unicode=True), encoding="utf-8")
PY
  echo "zksys-l2-bootstrap: updated ${zksys_contracts_yaml}: l2.zksys_gas_tank_addr=${ZKSYS_L2_GAS_TANK_ADDRESS}"
  echo "zksys-l2-bootstrap: address matches the canonical app binding"
  # SYSCOIN: The canonical main-node runner treats this attested nonzero value
  # as the durable transition out of its one-time first-boot exception.
  echo "zksys-l2-bootstrap: the next canonical edge-node launch will require the gas-tank runtime in local state"
else
  echo "zksys-l2-bootstrap: warning: ${zksys_contracts_yaml} not found; set l2.zksys_gas_tank_addr=${ZKSYS_L2_GAS_TANK_ADDRESS} manually" >&2
fi
