#!/usr/bin/env bash
# Smoke-check the deployed Syscoin Pali ERC-4337 stack.
#
# Required:
#   RPC_URL
#   ENTRYPOINT_ADDRESS
#   PAYMASTER_ADDRESS
#   PALI_ACCOUNT_IMPLEMENTATION_ADDRESS
#   PALI_ACCOUNT_FACTORY_ADDRESS
#   ZKSYS_TOKEN_ADDRESS
#
# Optional:
#   EXPECTED_ENTRYPOINT_CODEHASH
#   EXPECTED_PAYMASTER_CODEHASH
#   EXPECTED_PALI_ACCOUNT_IMPLEMENTATION_CODEHASH
#   EXPECTED_PALI_ACCOUNT_FACTORY_CODEHASH
#
set -euo pipefail

: "${RPC_URL:?RPC_URL is required}"
: "${ENTRYPOINT_ADDRESS:?ENTRYPOINT_ADDRESS is required}"
: "${PAYMASTER_ADDRESS:?PAYMASTER_ADDRESS is required}"
: "${PALI_ACCOUNT_IMPLEMENTATION_ADDRESS:?PALI_ACCOUNT_IMPLEMENTATION_ADDRESS is required}"
: "${PALI_ACCOUNT_FACTORY_ADDRESS:?PALI_ACCOUNT_FACTORY_ADDRESS is required}"
: "${ZKSYS_TOKEN_ADDRESS:?ZKSYS_TOKEN_ADDRESS is required}"

normalize_address() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

require_eq_address() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  if [[ "$(normalize_address "${actual}")" != "$(normalize_address "${expected}")" ]]; then
    echo "error: ${label}: got ${actual}, expected ${expected}" >&2
    exit 1
  fi
  echo "ok: ${label} = ${actual}"
}

require_code() {
  local label="$1"
  local address="$2"
  local code
  code="$(cast code "${address}" --rpc-url "${RPC_URL}")"
  if [[ "${code}" == "0x" ]]; then
    echo "error: ${label} has no code at ${address}" >&2
    exit 1
  fi
  echo "ok: ${label} has code at ${address}"
}

require_codehash_if_set() {
  local label="$1"
  local address="$2"
  local expected="$3"
  if [[ -z "${expected}" ]]; then
    return
  fi

  local code actual
  code="$(cast code "${address}" --rpc-url "${RPC_URL}")"
  actual="$(cast keccak "${code}")"
  if [[ "$(normalize_address "${actual}")" != "$(normalize_address "${expected}")" ]]; then
    echo "error: ${label} codehash: got ${actual}, expected ${expected}" >&2
    exit 1
  fi
  echo "ok: ${label} codehash = ${actual}"
}

require_code "SyscoinEntryPoint" "${ENTRYPOINT_ADDRESS}"
require_code "PaliFixedRateTokenPaymaster" "${PAYMASTER_ADDRESS}"
require_code "PaliSmartAccount implementation" "${PALI_ACCOUNT_IMPLEMENTATION_ADDRESS}"
require_code "PaliSmartAccountFactory" "${PALI_ACCOUNT_FACTORY_ADDRESS}"
require_code "zkSYS token" "${ZKSYS_TOKEN_ADDRESS}"

require_codehash_if_set "SyscoinEntryPoint" "${ENTRYPOINT_ADDRESS}" "${EXPECTED_ENTRYPOINT_CODEHASH:-}"
require_codehash_if_set "PaliFixedRateTokenPaymaster" "${PAYMASTER_ADDRESS}" "${EXPECTED_PAYMASTER_CODEHASH:-}"
require_codehash_if_set \
  "PaliSmartAccount implementation" \
  "${PALI_ACCOUNT_IMPLEMENTATION_ADDRESS}" \
  "${EXPECTED_PALI_ACCOUNT_IMPLEMENTATION_CODEHASH:-}"
require_codehash_if_set \
  "PaliSmartAccountFactory" \
  "${PALI_ACCOUNT_FACTORY_ADDRESS}" \
  "${EXPECTED_PALI_ACCOUNT_FACTORY_CODEHASH:-}"

paymaster_entrypoint="$(cast call "${PAYMASTER_ADDRESS}" "entryPoint()(address)" --rpc-url "${RPC_URL}")"
factory_entrypoint="$(cast call "${PALI_ACCOUNT_FACTORY_ADDRESS}" "entryPoint()(address)" --rpc-url "${RPC_URL}")"
implementation_entrypoint="$(cast call "${PALI_ACCOUNT_IMPLEMENTATION_ADDRESS}" "entryPoint()(address)" --rpc-url "${RPC_URL}")"
factory_implementation="$(cast call "${PALI_ACCOUNT_FACTORY_ADDRESS}" "implementation()(address)" --rpc-url "${RPC_URL}")"
entrypoint_paymaster="$(cast call "${ENTRYPOINT_ADDRESS}" "SYSCOIN_SPONSORED_PAYMASTER()(address)" --rpc-url "${RPC_URL}")"
burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "BURNER_ROLE()(bytes32)" --rpc-url "${RPC_URL}")"
has_burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "${burner_role}" "${PAYMASTER_ADDRESS}" --rpc-url "${RPC_URL}")"

require_eq_address "paymaster.entryPoint()" "${paymaster_entrypoint}" "${ENTRYPOINT_ADDRESS}"
require_eq_address "factory.entryPoint()" "${factory_entrypoint}" "${ENTRYPOINT_ADDRESS}"
require_eq_address "implementation.entryPoint()" "${implementation_entrypoint}" "${ENTRYPOINT_ADDRESS}"
require_eq_address "factory.implementation()" "${factory_implementation}" "${PALI_ACCOUNT_IMPLEMENTATION_ADDRESS}"
require_eq_address "entryPoint.SYSCOIN_SPONSORED_PAYMASTER()" "${entrypoint_paymaster}" "${PAYMASTER_ADDRESS}"

if [[ "${has_burner_role}" != "true" ]]; then
  echo "error: paymaster ${PAYMASTER_ADDRESS} does not have zkSYS BURNER_ROLE" >&2
  exit 1
fi
echo "ok: paymaster has zkSYS BURNER_ROLE"

echo "Pali deployment smoke check passed."
