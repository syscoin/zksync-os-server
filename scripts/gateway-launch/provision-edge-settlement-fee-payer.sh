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
readonly BPS_DENOMINATOR="10000"
readonly GAS_PRICE_HEADROOM_BPS="20000"
readonly GAS_LIMIT_HEADROOM_BPS="12500"

FEE_PAYER_KEYSTORE_DIR=""
FEE_PAYER_KEYSTORE_ACCOUNT="gateway-launch-edge-execute-operator"
FEE_PAYER_SIGNER_ARGS=()
FEE_PAYER_CHECK_ONLY=false
FEE_PAYER_VALIDATE_CONFIG_ONLY=false
FEE_PAYER_PREFLIGHT_FEE_TARGET=false

usage() {
  cat <<'EOF'
Usage: provision-edge-settlement-fee-payer.sh [--check-only] [EDGE_CHAIN_NAME ...]
       provision-edge-settlement-fee-payer.sh --validate-config-only
       provision-edge-settlement-fee-payer.sh --preflight-fee-target

With no arguments, EDGE_CHAIN_NAME (default: zksys) is provisioned. Each named
edge must already exist under GATEWAY_DIR/chains and settle on GATEWAY_RPC_URL.
Standalone/split-host use must set GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS from an
independently trusted deployment record, never from GATEWAY_RPC_URL itself.
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
    -u ETH_MAX_PRIORITY_FEE_PER_GAS -u ETH_GAS_LIMIT -u CAST_ASYNC cast "$@"
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

validate_fee_payer_config() {
  python3 - \
    "${GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET}" \
    "${GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI}" \
    "${GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI}" \
    "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" <<'PY'
import sys

UINT256_MAX = 2**256 - 1

def uint(raw: str, label: str, maximum: int) -> int:
    raw = raw.strip()
    try:
        value = int(raw, 16 if raw.lower().startswith("0x") else 10)
    except ValueError:
        raise SystemExit(f"invalid {label}: {raw!r}") from None
    if not 0 < value <= maximum:
        raise SystemExit(f"{label} must be between 1 and {maximum}")
    return value

uint(sys.argv[1], "GATEWAY_INTEROP_SETTLEMENT_OPERATION_BUDGET", UINT256_MAX)
uint(sys.argv[2], "GATEWAY_INTEROP_SETTLEMENT_MAX_WRAP_WEI", UINT256_MAX)
uint(sys.argv[3], "GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI", UINT256_MAX)
uint(sys.argv[4], "GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT", 86400)
PY
}

scale_uint_ceil() {
  python3 - "$1" "$2" "$3" <<'PY'
import sys

UINT256_MAX = 2**256 - 1
value, numerator, denominator = (int(raw, 10) for raw in sys.argv[1:])
if value < 0 or numerator < 0 or denominator <= 0:
    raise SystemExit("invalid unsigned scaling inputs")
scaled = (value * numerator + denominator - 1) // denominator
if scaled > UINT256_MAX:
    raise SystemExit("scaled transaction gas bound overflows uint256")
print(scaled)
PY
}

native_requirement_wei() {
  python3 - "$@" <<'PY'
import sys

UINT256_MAX = 2**256 - 1
deficit, reserve, max_fee, *gas_limits = (int(raw, 10) for raw in sys.argv[1:])
gas_units = sum(gas_limits)
if gas_units > UINT256_MAX or (max_fee and gas_units > UINT256_MAX // max_fee):
    raise SystemExit("bounded provisioning gas cost overflows uint256")
gas_budget = max_fee * gas_units
required = deficit + reserve + gas_budget
if required > UINT256_MAX:
    raise SystemExit("provisioning native requirement overflows uint256")
print(gas_budget, required)
PY
}

gateway_tx_gas_limit() {
  local operator_address="${1:?operator address required}"
  local max_fee_per_gas="${2:?max fee per gas required}"
  local label="${3:?gas estimate label required}"
  local estimate
  shift 3

  estimate="$(parse_uint \
    "$(gateway_cast estimate "$@" --from "${operator_address}" --gas-price "${max_fee_per_gas}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "${label}")"
  scale_uint_ceil "${estimate}" "${GAS_LIMIT_HEADROOM_BPS}" "${BPS_DENOMINATOR}"
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

live_gateway_settlement_fee() {
  local label="${1:-gatewaySettlementFee}"
  parse_uint \
    "$(gateway_cast call "${GW_ASSET_TRACKER_ADDRESS}" "gatewaySettlementFee()(uint256)" --rpc-url "${GATEWAY_RPC_URL}")" \
    "${label}"
}

preflight_live_settlement_target() {
  local live_fee target_wei
  address_has_code "${GW_ASSET_TRACKER_ADDRESS}" ||
    gl_die "GWAssetTracker has no code at ${GW_ASSET_TRACKER_ADDRESS}"
  live_fee="$(live_gateway_settlement_fee)" || return $?
  target_wei="$(settlement_target_wei "${live_fee}")" || return $?
  echo "gateway-launch: live Gateway settlement fee target is valid (fee=${live_fee}, target=${target_wei})"
}

provision_edge_fee_payer() {
  local edge_name="${1:?edge chain name required}"
  local wallet_path edge_config gateway_config edge_chain_id expected_gateway_chain_id
  local actual_gateway_chain_id operator_address edge_proxy wrapped_token live_fee target_wei
  local wrapped_balance deficit native_balance reserve required_native allowance agreement
  local gas_price max_fee_per_gas requirement provisioning_gas_budget
  local deposit_gas_limit approval_gas_limit agreement_gas_limit
  local latest_nonce pending_nonce
  local planned_transactions=()
  local final_fee final_target

  [[ "${edge_name}" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]] ||
    gl_die "invalid edge chain name: ${edge_name}"
  if [ "${FEE_PAYER_CHECK_ONLY}" != true ]; then
    gateway_acquire_execute_operator_lock "${edge_name}"
  fi

  edge_config="${GATEWAY_DIR}/chains/${edge_name}/ZkStack.yaml"
  gateway_config="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/ZkStack.yaml"
  wallet_path="${GATEWAY_DIR}/chains/${edge_name}/configs/wallets.yaml"
  [ -f "${wallet_path}" ] || gl_die "missing edge wallet file: ${wallet_path}"
  if [ "${FEE_PAYER_CHECK_ONLY}" = true ]; then
    gl_validate_secret_file "${wallet_path}" "edge wallet file"
  else
    gl_prepare_wallet_file_for_in_file "${wallet_path}"
  fi

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
  wrapped_token="${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}"
  address_has_code "${wrapped_token}" || gl_die "wrapped Gateway base token has no code at ${wrapped_token}"

  live_fee="$(live_gateway_settlement_fee)"
  target_wei="$(settlement_target_wei "${live_fee}")"
  reserve="$(parse_uint "${GATEWAY_INTEROP_SETTLEMENT_NATIVE_GAS_RESERVE_WEI}" "Gateway native gas reserve")"
  [ "${reserve}" != "0" ] || gl_die "Gateway native gas reserve must be non-zero"

  wrapped_balance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "balanceOf(address)(uint256)" "${operator_address}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator wrapped balance")"
  allowance="$(parse_uint \
    "$(gateway_cast call "${wrapped_token}" "allowance(address,address)(uint256)" "${operator_address}" "${GW_ASSET_TRACKER_ADDRESS}" --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator GWAssetTracker allowance")"
  agreement="$(gateway_cast call \
    "${GW_ASSET_TRACKER_ADDRESS}" \
    "settlementFeePayerAgreement(address,uint256)(bool)" \
    "${operator_address}" \
    "${edge_chain_id}" \
    --rpc-url "${GATEWAY_RPC_URL}" | awk '{print tolower($1)}')"

  deficit=0
  deposit_gas_limit=0
  approval_gas_limit=0
  agreement_gas_limit=0
  max_fee_per_gas=0
  if uint_lt "${wrapped_balance}" "${target_wei}"; then
    [ "${FEE_PAYER_CHECK_ONLY}" != true ] ||
      gl_die "${edge_name} execute_operator wrapped balance is below the live settlement target"
    deficit="$(python3 - "${target_wei}" "${wrapped_balance}" <<'PY'
import sys

print(int(sys.argv[1], 10) - int(sys.argv[2], 10))
PY
)"
    planned_transactions+=(deposit)
  fi
  if [ "${allowance}" != "${UINT256_MAX}" ]; then
    [ "${FEE_PAYER_CHECK_ONLY}" != true ] ||
      gl_die "${edge_name} execute_operator GWAssetTracker allowance is not ready"
    planned_transactions+=(approval)
  fi
  if [ "${agreement}" != "true" ]; then
    [ "${FEE_PAYER_CHECK_ONLY}" != true ] ||
      gl_die "${edge_name} execute_operator settlement-fee agreement is not enabled"
    planned_transactions+=(agreement)
  fi

  latest_nonce="$(parse_uint \
    "$(gateway_cast nonce "${operator_address}" --block latest --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator latest Gateway nonce")"
  pending_nonce="$(parse_uint \
    "$(gateway_cast nonce "${operator_address}" --block pending --rpc-url "${GATEWAY_RPC_URL}")" \
    "execute_operator pending Gateway nonce")"
  [ "${latest_nonce}" = "${pending_nonce}" ] ||
    gl_die "${edge_name} execute_operator ${operator_address} has pending Gateway transactions (latest nonce=${latest_nonce}, pending nonce=${pending_nonce}); wait for them before provisioning"

  if [ "${#planned_transactions[@]}" -gt 0 ]; then
    gas_price="$(parse_uint \
      "$(gateway_cast gas-price --rpc-url "${GATEWAY_RPC_URL}")" \
      "Gateway gas price")"
    max_fee_per_gas="$(scale_uint_ceil \
      "${gas_price}" "${GAS_PRICE_HEADROOM_BPS}" "${BPS_DENOMINATOR}")"
    if [ "${deficit}" != "0" ]; then
      deposit_gas_limit="$(gateway_tx_gas_limit \
        "${operator_address}" "${max_fee_per_gas}" "wrapped-token deposit gas estimate" \
        "${wrapped_token}" "deposit()" --value "${deficit}")"
    fi
    if [ "${allowance}" != "${UINT256_MAX}" ]; then
      approval_gas_limit="$(gateway_tx_gas_limit \
        "${operator_address}" "${max_fee_per_gas}" "GWAssetTracker approval gas estimate" \
        "${wrapped_token}" "approve(address,uint256)" "${GW_ASSET_TRACKER_ADDRESS}" "${UINT256_MAX}")"
    fi
    if [ "${agreement}" != "true" ]; then
      agreement_gas_limit="$(gateway_tx_gas_limit \
        "${operator_address}" "${max_fee_per_gas}" "settlement-fee agreement gas estimate" \
        "${GW_ASSET_TRACKER_ADDRESS}" "setSettlementFeePayerAgreement(uint256,bool)" "${edge_chain_id}" true)"
    fi

    requirement="$(native_requirement_wei \
      "${deficit}" "${reserve}" "${max_fee_per_gas}" \
      "${deposit_gas_limit}" "${approval_gas_limit}" "${agreement_gas_limit}")"
    read -r provisioning_gas_budget required_native <<<"${requirement}"
    native_balance="$(parse_uint \
      "$(gateway_cast balance "${operator_address}" --block latest --rpc-url "${GATEWAY_RPC_URL}")" \
      "execute_operator latest Gateway native balance")"
    if uint_lt "${native_balance}" "${required_native}"; then
      gl_die "${edge_name} execute_operator ${operator_address} needs ${required_native} Gateway native wei before broadcasting ${planned_transactions[*]} (wrap=${deficit}, retained reserve=${reserve}, bounded gas=${provisioning_gas_budget}); current=${native_balance}"
    fi

    # SYSCOIN: generated nodes and launch tools serialize this dedicated signer.
    # Bind the preflight liability to synchronous sends so provisioning cannot
    # consume the retained native reserve.
    prepare_execute_operator_keystore "${wallet_path}" "${operator_address}"
  fi

  if [ "${deficit}" != "0" ]; then
    echo "gateway-launch: wrapping ${deficit} Gateway base-token wei for ${edge_name} execute_operator ${operator_address}"
    gateway_cast send \
      "${wrapped_token}" \
      "deposit()" \
      --value "${deficit}" \
      --gas-limit "${deposit_gas_limit}" \
      --gas-price "${max_fee_per_gas}" \
      --rpc-url "${GATEWAY_RPC_URL}" \
      --confirmations 1 \
      --timeout "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" \
      "${FEE_PAYER_SIGNER_ARGS[@]}"
  fi

  if [ "${allowance}" != "${UINT256_MAX}" ]; then
    echo "gateway-launch: approving GWAssetTracker for ${edge_name} execute_operator ${operator_address}"
    gateway_cast send \
      "${wrapped_token}" \
      "approve(address,uint256)" \
      "${GW_ASSET_TRACKER_ADDRESS}" \
      "${UINT256_MAX}" \
      --gas-limit "${approval_gas_limit}" \
      --gas-price "${max_fee_per_gas}" \
      --rpc-url "${GATEWAY_RPC_URL}" \
      --confirmations 1 \
      --timeout "${GATEWAY_INTEROP_SETTLEMENT_TX_TIMEOUT}" \
      "${FEE_PAYER_SIGNER_ARGS[@]}"
  fi

  if [ "${agreement}" != "true" ]; then
    echo "gateway-launch: enabling Gateway settlement-fee agreement for ${edge_name} (${edge_chain_id}) execute_operator ${operator_address}"
    gateway_cast send \
      "${GW_ASSET_TRACKER_ADDRESS}" \
      "setSettlementFeePayerAgreement(uint256,bool)" \
      "${edge_chain_id}" \
      true \
      --gas-limit "${agreement_gas_limit}" \
      --gas-price "${max_fee_per_gas}" \
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
  final_fee="$(live_gateway_settlement_fee "gatewaySettlementFee after provisioning")"
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
  if [ "${1:-}" = "--validate-config-only" ]; then
    FEE_PAYER_VALIDATE_CONFIG_ONLY=true
    shift
  elif [ "${1:-}" = "--preflight-fee-target" ]; then
    FEE_PAYER_PREFLIGHT_FEE_TARGET=true
    shift
  elif [ "${1:-}" = "--check-only" ]; then
    FEE_PAYER_CHECK_ONLY=true
    shift
  fi
  if [ "$#" -eq 1 ] && { [ "$1" = "-h" ] || [ "$1" = "--help" ]; }; then
    usage
    return 0
  fi
  validate_fee_payer_config
  if [ "${FEE_PAYER_VALIDATE_CONFIG_ONLY}" = true ]; then
    [ "$#" -eq 0 ] || gl_die "--validate-config-only does not accept edge names"
    return 0
  fi
  if [ "${FEE_PAYER_PREFLIGHT_FEE_TARGET}" = true ]; then
    [ "$#" -eq 0 ] || gl_die "--preflight-fee-target does not accept edge names"
  fi
  if [ "$#" -eq 0 ]; then
    edge_names=("${EDGE_CHAIN_NAME:-zksys}")
  else
    edge_names=("$@")
  fi

  command -v cast >/dev/null 2>&1 || gl_die "cast is required"
  if [ "${FEE_PAYER_CHECK_ONLY}" != true ] &&
    [ "${FEE_PAYER_PREFLIGHT_FEE_TARGET}" != true ]; then
    command -v openssl >/dev/null 2>&1 || gl_die "openssl is required"
  fi
  # SYSCOIN: This standalone signing entry point must authenticate the same
  # immutable Gateway deployment stamp as edge creation and migration helpers.
  gl_assert_gateway_runtime_identity
  # SYSCOIN: the native-value transaction target must be independently pinned;
  # same-RPC code and postcondition reads cannot authenticate a dynamic target.
  gl_assert_gateway_wrapped_base_token_pin "${GATEWAY_RPC_URL}"
  if [ "${FEE_PAYER_PREFLIGHT_FEE_TARGET}" = true ]; then
    preflight_live_settlement_target
    return 0
  fi
  for edge_name in "${edge_names[@]}"; do
    provision_edge_fee_payer "${edge_name}"
  done
}

main "$@"
