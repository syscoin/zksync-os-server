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
: "${ZKSYNC_ERA_PATH:=$(cd "${ZKSYNC_OS_SERVER_PATH}/.." && pwd)/zksync-era}"

: "${ZKSYS_L2_CREATE2_DEPLOYER:=0x4e59b44847b379578588920cA78FbF26c0B4956C}"
: "${ZKSYS_L2_TOKEN_NAME:=ZKSYS}"
: "${ZKSYS_L2_TOKEN_SYMBOL:=ZKSYS}"
: "${ZKSYS_L2_TOKEN_DECIMALS:=18}"
: "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS:=0x0000000000000000000000000000000000000000}"
: "${ZKSYS_L2_PAYMASTER_ADDRESS:=}"
: "${ZKSYS_ISSUER_PERIOD_SECONDS:=86400}"
: "${ZKSYS_ISSUER_PERIODS_PER_YEAR:=365}"
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

require_create2_deployer() {
  local code
  code="$(rpc_code "${ZKSYS_L2_CREATE2_DEPLOYER}")"
  [ "${code}" != "0x" ] || gl_die "CREATE2 deployer has no code at ${ZKSYS_L2_CREATE2_DEPLOYER}"
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

forge_inspect_bytecode() {
  local contract="${1:?contract required}"
  forge inspect "${contract}" bytecode \
    --no-metadata \
    --root "${inspect_dir}" \
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
    ZKSYS_L2_CAST_WALLET_ARGS+=(--account "${account_name}")
    ;;
  keystore)
    keystore_path="${ZKSYS_L2_DEPLOYER_KEYSTORE:-${DEPLOYER_KEYSTORE:-${FUNDER_KEYSTORE:-}}}"
    [ -n "${keystore_path}" ] || gl_die "ZKSYS_L2_DEPLOYER_KEYSTORE is required when ZKSYS_L2_DEPLOYER_SIGNER=keystore"
    [ -f "${keystore_path}" ] || gl_die "ZKSYS_L2_DEPLOYER_KEYSTORE does not exist: ${keystore_path}"
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
    [ -f "${password_file}" ] || gl_die "ZKSYS_L2_DEPLOYER_PASSWORD_FILE does not exist: ${password_file}"
    ZKSYS_L2_CAST_WALLET_ARGS+=(--password-file "${password_file}")
  fi
}

ZKSYS_L2_CREATE2_DEPLOYER="$(normalize_nonzero_address_env ZKSYS_L2_CREATE2_DEPLOYER)"
ZKSYS_L2_TOKEN_ADMIN_ADDRESS="$(normalize_nonzero_address_env ZKSYS_L2_TOKEN_ADMIN_ADDRESS)"
ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS="$(normalize_address_env ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS)"
if [ -n "${ZKSYS_L2_PAYMASTER_ADDRESS}" ]; then
  ZKSYS_L2_PAYMASTER_ADDRESS="$(normalize_nonzero_address_env ZKSYS_L2_PAYMASTER_ADDRESS)"
fi
export ZKSYS_L2_CREATE2_DEPLOYER
export ZKSYS_L2_TOKEN_ADMIN_ADDRESS
export ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS
export ZKSYS_L2_PAYMASTER_ADDRESS

case "${ZKSYS_L2_TOKEN_DECIMALS}" in
''|*[!0-9]*) gl_die "ZKSYS_L2_TOKEN_DECIMALS must be a uint8" ;;
esac
[ "${ZKSYS_L2_TOKEN_DECIMALS}" -le 255 ] || gl_die "ZKSYS_L2_TOKEN_DECIMALS must be <= 255"
for schedule_var in ZKSYS_ISSUER_START_TIME ZKSYS_ISSUER_PERIOD_SECONDS ZKSYS_ISSUER_PERIODS_PER_YEAR; do
  case "${!schedule_var}" in
  ''|*[!0-9]*) gl_die "${schedule_var} must be a decimal uint256" ;;
  esac
done
[ "${ZKSYS_ISSUER_PERIOD_SECONDS}" != "0" ] || gl_die "ZKSYS_ISSUER_PERIOD_SECONDS must be non-zero"
[ "${ZKSYS_ISSUER_PERIODS_PER_YEAR}" != "0" ] || gl_die "ZKSYS_ISSUER_PERIODS_PER_YEAR must be non-zero"
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
ZKSYS_L2_REGISTRY_SALT="$(normalize_bytes32_env ZKSYS_L2_REGISTRY_SALT 0x7a6b7379732d7265676973747279000000000000000000000000000000000000)"
ZKSYS_L2_WEIGHT_REGISTRY_SALT="$(normalize_bytes32_env ZKSYS_L2_WEIGHT_REGISTRY_SALT 0x7a6b7379732d7765696768742d72656769737472790000000000000000000000)"
ZKSYS_L2_ISSUER_SALT="$(normalize_bytes32_env ZKSYS_L2_ISSUER_SALT 0x7a6b7379732d6973737565720000000000000000000000000000000000000000)"

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

registry_ctor_args="$(
  cast abi-encode \
    "constructor(address,address)" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZERO_ADDRESS}"
)"
registry_init_code="$(forge_inspect_bytecode ZkSysMembershipRegistry)${registry_ctor_args#0x}"
ZKSYS_L2_REGISTRY_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_REGISTRY_SALT}" \
    --init-code "${registry_init_code}"
)"

weight_registry_ctor_args="$(
  cast abi-encode \
    "constructor(address,address)" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZKSYS_L2_REGISTRY_ADDRESS}"
)"
weight_registry_init_code="$(forge_inspect_bytecode ZkSysRewardWeightRegistry)${weight_registry_ctor_args#0x}"
ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_WEIGHT_REGISTRY_SALT}" \
    --init-code "${weight_registry_init_code}"
)"

issuer_ctor_args="$(
  cast abi-encode \
    "constructor(address,address,address,uint256,uint256,uint256)" \
    "${ZKSYS_L2_TOKEN_ADDRESS}" \
    "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" \
    "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" \
    "${ZKSYS_ISSUER_START_TIME}" \
    "${ZKSYS_ISSUER_PERIOD_SECONDS}" \
    "${ZKSYS_ISSUER_PERIODS_PER_YEAR}"
)"
issuer_init_code="$(forge_inspect_bytecode ZkSysIssuer)${issuer_ctor_args#0x}"
ZKSYS_L2_ISSUER_ADDRESS="$(
  cast create2 \
    --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" \
    --salt "${ZKSYS_L2_ISSUER_SALT}" \
    --init-code "${issuer_init_code}"
)"

require_create2_deployer
deploy_create2 "zkSYS proxy admin" "${ZKSYS_L2_PROXY_ADMIN_ADDRESS}" "${ZKSYS_L2_PROXY_ADMIN_SALT}" "${proxy_admin_init_code}"
deploy_create2 "zkSYS token implementation" "${ZKSYS_L2_TOKEN_IMPL_ADDRESS}" "${ZKSYS_L2_TOKEN_IMPL_SALT}" "${token_impl_init_code}"
deploy_create2 "zkSYS token proxy" "${ZKSYS_L2_TOKEN_ADDRESS}" "${ZKSYS_L2_TOKEN_PROXY_SALT}" "${token_proxy_init_code}"
deploy_create2 "zkSYS membership registry" "${ZKSYS_L2_REGISTRY_ADDRESS}" "${ZKSYS_L2_REGISTRY_SALT}" "${registry_init_code}"
deploy_create2 "zkSYS reward weight registry" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "${ZKSYS_L2_WEIGHT_REGISTRY_SALT}" "${weight_registry_init_code}"
deploy_create2 "zkSYS issuer" "${ZKSYS_L2_ISSUER_ADDRESS}" "${ZKSYS_L2_ISSUER_SALT}" "${issuer_init_code}"

MINTER_ROLE="$(cast keccak "$(cast from-ascii MINTER_ROLE)")"
BURNER_ROLE="$(cast keccak "$(cast from-ascii BURNER_ROLE)")"

echo "zksys-l2-bootstrap: wiring issuer minter role and registry receivers"
send_l2 "${ZKSYS_L2_TOKEN_ADDRESS}" "grantRole(bytes32,address)" "${MINTER_ROLE}" "${ZKSYS_L2_ISSUER_ADDRESS}"
send_l2 "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" "setWeightReceiver(address)" "${ZKSYS_L2_ISSUER_ADDRESS}"
send_l2 "${ZKSYS_L2_REGISTRY_ADDRESS}" "setSentryNodeReceiver(address)" "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}"
if [ "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" != "${ZERO_ADDRESS}" ]; then
  send_l2 "${ZKSYS_L2_REGISTRY_ADDRESS}" "setL1RegistryBridge(address)" "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}"
fi

if [ -n "${ZKSYS_L2_PAYMASTER_ADDRESS}" ]; then
  echo "zksys-l2-bootstrap: wiring paymaster burner role"
  send_l2 "${ZKSYS_L2_TOKEN_ADDRESS}" "grantRole(bytes32,address)" "${BURNER_ROLE}" "${ZKSYS_L2_PAYMASTER_ADDRESS}"
fi

cat <<EOF
zksys-l2-bootstrap: complete
  proxyAdmin          = ${ZKSYS_L2_PROXY_ADMIN_ADDRESS}
  tokenImplementation = ${ZKSYS_L2_TOKEN_IMPL_ADDRESS}
  tokenProxy          = ${ZKSYS_L2_TOKEN_ADDRESS}
  registry            = ${ZKSYS_L2_REGISTRY_ADDRESS}
  weightRegistry      = ${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}
  issuer              = ${ZKSYS_L2_ISSUER_ADDRESS}
EOF
