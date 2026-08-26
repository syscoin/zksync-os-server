#!/usr/bin/env bash
# Provision each edge execute operator to pay Gateway interop settlement fees.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_execute_operator_lock.sh"

: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${GATEWAY_RPC_URL:=http://127.0.0.1:${GATEWAY_OS_RPC_PORT:-3052}}"
: "${GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET:=5}"
: "${GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI:=1000000000000000000000}"
: "${GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI:=10000000000000000000}"
: "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT:=120}"

# SYSCOIN: V32 Gateway charges interop settlement fees through this system contract.
readonly GW_ASSET_TRACKER_ADDRESS="0x0000000000000000000000000000000000010010"
readonly GATEWAY_BRIDGEHUB_ADDRESS="0x0000000000000000000000000000000000010002"
readonly UINT256_MAX="115792089237316195423570985008687907853269984665640564039457584007913129639935"

FEE_PAYER_KEYSTORE_DIR=""
FEE_PAYER_KEYSTORE_ACCOUNT="gateway-launch-edge-execute-operator"
FEE_PAYER_SIGNER_ARGS=()

usage() {
  cat <<'EOF'
Usage: provision-edge-settlement-fee-payer.sh [EDGE_CHAIN_NAME ...]

With no arguments, EDGE_CHAIN_NAME (default: zksys) is provisioned. Each named
edge must already exist under GATEWAY_DIR/chains and settle on GATEWAY_RPC_URL.
EOF
}

cleanup_fee_payer_keystore() {
  if [ -n "${FEE_PAYER_KEYSTORE_DIR:-}" ] && [ -d "${FEE_PAYER_KEYSTORE_DIR}" ]; then
    rm -rf -- "${FEE_PAYER_KEYSTORE_DIR}"
  fi
  FEE_PAYER_KEYSTORE_DIR=""
  FEE_PAYER_SIGNER_ARGS=()
}

cleanup_fee_payer_state() {
  cleanup_fee_payer_keystore
  gateway_release_execute_operator_lock
}
trap cleanup_fee_payer_state EXIT

gateway_cast() {
  # SYSCOIN: Gateway is a distinct chain. Do not leak L1 chain/fee overrides into
  # read calls or signed Gateway transactions.
  env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    -u ETH_GAS_PRICE -u ETH_PRIORITY_GAS_PRICE -u ETH_MAX_FEE_PER_GAS \
    -u ETH_MAX_PRIORITY_FEE_PER_GAS cast "$@"
}

require_address() {
  python3 - "$1" "$2" <<'PY'
import re
import sys

value = sys.argv[1].strip().split()[0] if sys.argv[1].strip() else ""
label = sys.argv[2]
if not re.fullmatch(r"0x[0-9a-fA-F]{40}", value):
    raise SystemExit(f"invalid {label}: {value!r}")
if int(value[2:], 16) == 0:
    raise SystemExit(f"{label} must be non-zero")
print(value)
PY
}

parse_uint() {
  python3 - "$1" "$2" <<'PY'
import sys

raw = sys.argv[1].strip()
label = sys.argv[2]
if not raw:
    raise SystemExit(f"missing {label}")
token = raw.split()[0]
try:
    value = int(token, 16 if token.lower().startswith("0x") else 10)
except ValueError:
    raise SystemExit(f"invalid {label}: {token!r}") from None
if value < 0 or value >= 2**256:
    raise SystemExit(f"{label} is outside uint256")
print(value)
PY
}

uint_lt() {
  python3 - "$1" "$2" <<'PY'
import sys

raise SystemExit(0 if int(sys.argv[1], 10) < int(sys.argv[2], 10) else 1)
PY
}

uint_ge() {
  python3 - "$1" "$2" <<'PY'
import sys

raise SystemExit(0 if int(sys.argv[1], 10) >= int(sys.argv[2], 10) else 1)
PY
}

chain_id_from_yaml() {
  python3 - "$1" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
if not path.is_file():
    raise SystemExit(f"missing chain config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
value = data.get("chain_id") if isinstance(data, dict) else None
if isinstance(value, str):
    value = int(value, 16 if value.lower().startswith("0x") else 10)
if not isinstance(value, int) or isinstance(value, bool) or value <= 0 or value >= 2**256:
    raise SystemExit(f"invalid chain_id in {path}")
print(value)
PY
}

execute_operator_address() {
  python3 - "$1" <<'PY'
import re
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8"))
wallet = data.get("execute_operator") if isinstance(data, dict) else None
value = wallet.get("address") if isinstance(wallet, dict) else None
if isinstance(value, int) and not isinstance(value, bool):
    if value <= 0 or value >= 2**160:
        raise SystemExit(f"invalid execute_operator.address in {path}")
    value = "0x" + format(value, "040x")
if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]{40}", value.strip()):
    raise SystemExit(f"missing or invalid execute_operator.address in {path}")
if int(value.strip()[2:], 16) == 0:
    raise SystemExit(f"execute_operator.address is zero in {path}")
print(value.strip())
PY
}

prepare_execute_operator_keystore() {
  local wallet_path="${1:?wallet path required}"
  local expected_address="${2:?expected address required}"
  local imported_address password_file

  # SYSCOIN: generated edge wallets store the execute key in YAML. Import it
  # through hidden prompts into an ephemeral encrypted keystore; never place the
  # key in the process argv, environment, or launcher output.
  cleanup_fee_payer_keystore
  FEE_PAYER_KEYSTORE_DIR="$(mktemp -d)"
  chmod 700 "${FEE_PAYER_KEYSTORE_DIR}"
  password_file="${FEE_PAYER_KEYSTORE_DIR}/password"
  (umask 077 && openssl rand -hex 32 >"${password_file}")

  command -v expect >/dev/null 2>&1 ||
    gl_die "expect is required to import execute_operator without exposing its private key in argv"

  WALLET_PATH="${wallet_path}" \
    KEYSTORE_DIR="${FEE_PAYER_KEYSTORE_DIR}" \
    KEYSTORE_PASSWORD_FILE="${password_file}" \
    CAST_BIN="$(command -v cast)" \
    ACCOUNT_NAME="${FEE_PAYER_KEYSTORE_ACCOUNT}" \
    expect <<'EXPECT'
set timeout 60
log_user 0
set pk [exec bash -c {python3 - "$WALLET_PATH" <<'PY'
import re
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8"))
wallet = data.get("execute_operator") if isinstance(data, dict) else None
value = wallet.get("private_key") if isinstance(wallet, dict) else None
if isinstance(value, int) and not isinstance(value, bool):
    if value <= 0 or value >= 2**256:
        raise SystemExit(f"invalid execute_operator.private_key in {path}")
    value = "0x" + format(value, "064x")
if isinstance(value, str):
    value = value.strip()
    if value.lower().startswith("0x"):
        value = "0x" + value[2:]
    else:
        value = "0x" + value
if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]{64}", value):
    raise SystemExit(f"missing or invalid execute_operator.private_key in {path}")
if int(value[2:], 16) == 0:
    raise SystemExit(f"execute_operator.private_key is zero in {path}")
print(value)
PY
}]
set pw [exec sh -c {tr -d '\n' < "$KEYSTORE_PASSWORD_FILE"}]
spawn $env(CAST_BIN) wallet import $env(ACCOUNT_NAME) --keystore-dir $env(KEYSTORE_DIR) --interactive
expect -re "(?i).*private key.*"
send -- "$pk\r"
expect {
  -re "(?i).*password.*" {
    send -- "$pw\r"
    expect {
      -re "(?i).*(confirm|repeat|re-enter).*password.*" {
        send -- "$pw\r"
        expect eof
      }
      eof {}
    }
  }
  eof {}
}
EXPECT

  imported_address="$(
    cast wallet address \
      --keystore "${FEE_PAYER_KEYSTORE_DIR}/${FEE_PAYER_KEYSTORE_ACCOUNT}" \
      --password-file "${password_file}"
  )"
  if [ "$(gl_to_lower "${imported_address}")" != "$(gl_to_lower "${expected_address}")" ]; then
    gl_die "execute_operator keystore mismatch: expected ${expected_address}, got ${imported_address}"
  fi

  FEE_PAYER_SIGNER_ARGS=(
    --keystore "${FEE_PAYER_KEYSTORE_DIR}/${FEE_PAYER_KEYSTORE_ACCOUNT}"
    --password-file "${password_file}"
  )
}

address_has_code() {
  local code
  code="$(gateway_cast code "$1" --rpc-url "${GATEWAY_RPC_URL}")"
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ -n "${code}" ] && [ "${code}" != "0x" ]
}

settlement_target_wei() {
  local live_fee="${1:?live fee required}"
  python3 - \
    "${live_fee}" \
    "${GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET}" \
    "${GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI}" <<'PY'
import sys

UINT256_MAX = 2**256 - 1

def number(raw: str, label: str) -> int:
    raw = raw.strip()
    try:
        value = int(raw, 16 if raw.lower().startswith("0x") else 10)
    except ValueError:
        raise SystemExit(f"invalid {label}: {raw!r}") from None
    if value < 0 or value > UINT256_MAX:
        raise SystemExit(f"{label} is outside uint256")
    return value

fee = number(sys.argv[1], "gatewaySettlementFee")
budget = number(sys.argv[2], "GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET")
cap = number(sys.argv[3], "GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI")
if fee == 0:
    raise SystemExit("gatewaySettlementFee must be non-zero before fee-payer provisioning")
if budget == 0:
    raise SystemExit("GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET must be non-zero")
if cap == 0:
    raise SystemExit("GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI must be non-zero")
if fee > UINT256_MAX // budget:
    raise SystemExit("gateway settlement fee target overflows uint256")
target = fee * budget
if target > cap:
    raise SystemExit(
        f"gateway settlement fee target {target} exceeds configured wrap cap {cap}"
    )
print(target)
PY
}

provision_edge_fee_payer() {
  local edge_name="${1:?edge chain name required}"
  local wallet_path edge_config gateway_config edge_chain_id expected_gateway_chain_id
  local actual_gateway_chain_id operator_address edge_proxy wrapped_token live_fee target_wei
  local wrapped_balance deficit native_balance reserve required_native allowance agreement
  local final_fee final_target

  [[ "${edge_name}" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]] ||
    gl_die "invalid edge chain name: ${edge_name}"
  gateway_acquire_execute_operator_lock "${edge_name}"

  edge_config="${GATEWAY_DIR}/chains/${edge_name}/ZkStack.yaml"
  gateway_config="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/ZkStack.yaml"
  wallet_path="${GATEWAY_DIR}/chains/${edge_name}/configs/wallets.yaml"
  [ -f "${wallet_path}" ] || gl_die "missing edge wallet file: ${wallet_path}"
  gl_prepare_wallet_file_for_in_file "${wallet_path}"

  edge_chain_id="$(chain_id_from_yaml "${edge_config}")"
  expected_gateway_chain_id="$(chain_id_from_yaml "${gateway_config}")"
  actual_gateway_chain_id="$(parse_uint "$(gateway_cast chain-id --rpc-url "${GATEWAY_RPC_URL}")" "Gateway RPC chain ID")"
  if [ "${actual_gateway_chain_id}" != "${expected_gateway_chain_id}" ]; then
    gl_die "Gateway RPC chain ID ${actual_gateway_chain_id} does not match ${GATEWAY_CHAIN_NAME} chain ID ${expected_gateway_chain_id}"
  fi

  operator_address="$(execute_operator_address "${wallet_path}")"
  edge_proxy="$(require_address \
    "$(gateway_cast call "${GATEWAY_BRIDGEHUB_ADDRESS}" "getZKChain(uint256)(address)" "${edge_chain_id}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "Gateway Bridgehub edge proxy for ${edge_name}")"
  address_has_code "${edge_proxy}" ||
    gl_die "Gateway Bridgehub edge proxy for ${edge_name} has no code at ${edge_proxy}"
  address_has_code "${GW_ASSET_TRACKER_ADDRESS}" ||
    gl_die "GWAssetTracker has no code at ${GW_ASSET_TRACKER_ADDRESS}"
  wrapped_token="$(require_address \
    "$(gateway_cast call "${GW_ASSET_TRACKER_ADDRESS}" "wrappedZKToken()(address)" --rpc-url "${GATEWAY_RPC_URL}")" \
    "GWAssetTracker wrappedZKToken")"
  address_has_code "${wrapped_token}" || gl_die "wrapped Gateway base token has no code at ${wrapped_token}"

  live_fee="$(parse_uint \
    "$(gateway_cast call "${GW_ASSET_TRACKER_ADDRESS}" "gatewaySettlementFee()(uint256)" --rpc-url "${GATEWAY_RPC_URL}")" \
    "gatewaySettlementFee")"
  target_wei="$(settlement_target_wei "${live_fee}")"
  reserve="$(parse_uint "${GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI}" "Gateway native gas reserve")"
  [ "${reserve}" != "0" ] || gl_die "Gateway native gas reserve must be non-zero"

  wrapped_balance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "balanceOf(address)(uint256)" "${operator_address}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator wrapped balance")"
  if uint_lt "${wrapped_balance}" "${target_wei}"; then
    deficit="$(python3 - "${target_wei}" "${wrapped_balance}" <<'PY'
import sys

print(int(sys.argv[1], 10) - int(sys.argv[2], 10))
PY
)"
    native_balance="$(parse_uint \
      "$(gateway_cast balance "${operator_address}" --rpc-url "${GATEWAY_RPC_URL}")" \
      "execute_operator Gateway native balance")"
    required_native="$(python3 - "${deficit}" "${reserve}" <<'PY'
import sys

deficit = int(sys.argv[1], 10)
reserve = int(sys.argv[2], 10)
if deficit > 2**256 - 1 - reserve:
    raise SystemExit("wrapped-token deficit plus native gas reserve overflows uint256")
print(deficit + reserve)
PY
)"
    if uint_lt "${native_balance}" "${required_native}"; then
      gl_die "${edge_name} execute_operator ${operator_address} needs ${required_native} Gateway native wei to wrap ${deficit} wei while retaining the ${reserve} wei gas reserve; current=${native_balance}"
    fi

    prepare_execute_operator_keystore "${wallet_path}" "${operator_address}"
    echo "gateway-launch: wrapping ${deficit} Gateway base-token wei for ${edge_name} execute_operator ${operator_address}"
    gateway_cast send \
      "${wrapped_token}" \
      "deposit()" \
      --value "${deficit}" \
      --rpc-url "${GATEWAY_RPC_URL}" \
      --confirmations 1 \
      --timeout "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" \
      "${FEE_PAYER_SIGNER_ARGS[@]}"
  fi

  allowance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "allowance(address,address)(uint256)" "${operator_address}" "${GW_ASSET_TRACKER_ADDRESS}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator GWAssetTracker allowance")"
  if [ "${allowance}" != "${UINT256_MAX}" ]; then
    [ "${#FEE_PAYER_SIGNER_ARGS[@]}" -gt 0 ] ||
      prepare_execute_operator_keystore "${wallet_path}" "${operator_address}"
    echo "gateway-launch: approving GWAssetTracker for ${edge_name} execute_operator ${operator_address}"
    gateway_cast send \
      "${wrapped_token}" \
      "approve(address,uint256)" \
      "${GW_ASSET_TRACKER_ADDRESS}" \
      "${UINT256_MAX}" \
      --rpc-url "${GATEWAY_RPC_URL}" \
      --confirmations 1 \
      --timeout "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" \
      "${FEE_PAYER_SIGNER_ARGS[@]}"
  fi

  agreement="$(gateway_cast call \
    "${GW_ASSET_TRACKER_ADDRESS}" \
    "settlementFeePayerAgreement(address,uint256)(bool)" \
    "${operator_address}" \
    "${edge_chain_id}" \
    --rpc-url "${GATEWAY_RPC_URL}" | awk '{print tolower($1)}')"
  if [ "${agreement}" != "true" ]; then
    [ "${#FEE_PAYER_SIGNER_ARGS[@]}" -gt 0 ] ||
      prepare_execute_operator_keystore "${wallet_path}" "${operator_address}"
    echo "gateway-launch: enabling Gateway settlement-fee agreement for ${edge_name} (${edge_chain_id}) execute_operator ${operator_address}"
    gateway_cast send \
      "${GW_ASSET_TRACKER_ADDRESS}" \
      "setSettlementFeePayerAgreement(uint256,bool)" \
      "${edge_chain_id}" \
      true \
      --rpc-url "${GATEWAY_RPC_URL}" \
      --confirmations 1 \
      --timeout "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" \
      "${FEE_PAYER_SIGNER_ARGS[@]}"
  fi

  wrapped_balance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "balanceOf(address)(uint256)" "${operator_address}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator wrapped balance after provisioning")"
  allowance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "allowance(address,address)(uint256)" "${operator_address}" "${GW_ASSET_TRACKER_ADDRESS}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator GWAssetTracker allowance after provisioning")"
  agreement="$(gateway_cast call \
    "${GW_ASSET_TRACKER_ADDRESS}" \
    "settlementFeePayerAgreement(address,uint256)(bool)" \
    "${operator_address}" \
    "${edge_chain_id}" \
    --rpc-url "${GATEWAY_RPC_URL}" | awk '{print tolower($1)}')"
  final_fee="$(parse_uint \
    "$(gateway_cast call "${GW_ASSET_TRACKER_ADDRESS}" "gatewaySettlementFee()(uint256)" --rpc-url "${GATEWAY_RPC_URL}")" \
    "gatewaySettlementFee after provisioning")"
  final_target="$(settlement_target_wei "${final_fee}")"
  native_balance="$(parse_uint \
    "$(gateway_cast balance "${operator_address}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator Gateway native balance after provisioning")"
  uint_ge "${wrapped_balance}" "${final_target}" ||
    gl_die "wrapped Gateway base-token balance verification failed for ${edge_name}"
  uint_ge "${native_balance}" "${reserve}" ||
    gl_die "Gateway native gas reserve verification failed for ${edge_name}: required=${reserve} current=${native_balance}"
  [ "${allowance}" = "${UINT256_MAX}" ] ||
    gl_die "GWAssetTracker allowance verification failed for ${edge_name}"
  [ "${agreement}" = "true" ] ||
    gl_die "Gateway settlement-fee agreement verification failed for ${edge_name}"

  echo "gateway-launch: ${edge_name} execute_operator can fund ${GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET} chargeable interop operations at live fee ${final_fee} wei (wrapped target=${final_target} wei)"
  cleanup_fee_payer_state
}

main() {
  local edge_names=()
  if [ "$#" -eq 1 ] && { [ "$1" = "-h" ] || [ "$1" = "--help" ]; }; then
    usage
    return 0
  fi
  if [ "$#" -eq 0 ]; then
    edge_names=("${EDGE_CHAIN_NAME:-zksys}")
  else
    edge_names=("$@")
  fi

  command -v cast >/dev/null 2>&1 || gl_die "cast is required"
  command -v openssl >/dev/null 2>&1 || gl_die "openssl is required"
  # SYSCOIN: This standalone signing entry point must authenticate the same
  # immutable Gateway deployment stamp as edge creation and migration helpers.
  gl_assert_gateway_runtime_identity
  for edge_name in "${edge_names[@]}"; do
    provision_edge_fee_payer "${edge_name}"
  done
}

main "$@"
