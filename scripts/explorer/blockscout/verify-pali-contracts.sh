#!/usr/bin/env bash
# Verify the zkSYS Pali ERC-4337 contracts on a Blockscout instance.
#
# This is reproducible and idempotent: it verifies the canonical ERC-4337
# EntryPoint v0.9 singleton and the deterministic Pali smart-account
# infrastructure. There is no paymaster; zkSYS gas payment is native to the
# chain via the ZkSysGasTank.
#
# Prerequisites on the target Blockscout instance (already wired in
# docker-compose.yml): the smart-contract-verifier microservice must be running
# and the backend must have MICROSERVICE_SC_VERIFIER_{ENABLED,URL,TYPE} set.
#
# Deterministic addresses are derived the same way as
# pali-wallet/source/utils/smartAccount/deployment.ts: CREATE2 through the
# 0x4e59...4956C deployer with the PALI_SMART_ACCOUNT_ERC7579_V1 salts.
#
# Usage:
#   ./verify-pali-contracts.sh [EXPLORER_BASE_URL]
# Defaults to the zkTanenbaum public explorer.

set -euo pipefail

EXPLORER_BASE="${1:-https://explorer-zk.tanenbaum.io}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"
AA_DIR="${REPO_ROOT}/integration-tests/test-contracts/lib/account-abstraction"

PALI_SOLC="v0.8.28+commit.7893614a"
PALI_SOLC_VERSION="${PALI_SOLC_VERSION:-0.8.28}"
CHAIN_ID="${CHAIN_ID:-57057}"
RPC_URL="${ZKTANENBAUM_RPC_URL:-${RPC_URL:-https://rpc-zk.tanenbaum.io}}"
# Canonical ERC-4337 EntryPoint v0.9 singleton.
ENTRYPOINT_ADDRESS="${ENTRYPOINT_ADDRESS:-0x433709009B8330FDa32311DF1C2AFA402eD8D009}"
PALI_CREATE2_DEPLOYER_ADDRESS="${PALI_CREATE2_DEPLOYER_ADDRESS:-0x4e59b44847b379578588920cA78FbF26c0B4956C}"
PALI_INFRASTRUCTURE_VERSION="PALI_SMART_ACCOUNT_ERC7579_V1"

export FOUNDRY_BYTECODE_HASH=none
export FOUNDRY_CBOR_METADATA=false

# SLH-DSA validator constructor args: abi.encode(verifier).
SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS="0x000000000000000000000000789d5ac3a14b543a46fc402eedcf31d8c8b93d4a31e33d9848db6a8821cf39adeb347aff047a308f52b04aee2a398e29fee8b628"

lower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

forge_inspect_bytecode() {
  (
    cd "${CONTRACTS_DIR}"
    forge inspect --use "${PALI_SOLC_VERSION}" --no-auto-detect --no-metadata "$1" bytecode
  )
}

abi_encode() {
  cast abi-encode "$@"
}

salt() {
  cast keccak "${PALI_INFRASTRUCTURE_VERSION}:$1"
}

create2_address() {
  cast create2 \
    --deployer "${PALI_CREATE2_DEPLOYER_ADDRESS}" \
    --salt "$1" \
    --init-code "$2"
}

runtime_code() {
  cast code "$1" --rpc-url "${RPC_URL}"
}

is_verified() {
  curl -s "${EXPLORER_BASE}/api/v2/smart-contracts/$1" \
    | grep -q '"is_verified":true'
}

verify_contract() {
  local addr="$1"
  local label="$2"
  local contract="$3"
  local ctor="${4:-}"

  if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
    echo "missing ${label} (${addr}); skipping verification because no code is deployed" >&2
    return
  fi

  if is_verified "${addr}"; then
    echo "skip   ${label} (${addr}) already verified"
    return
  fi

  echo "submit ${label} (${addr})"
  (
    cd "${CONTRACTS_DIR}"
    args=(
      verify-contract
      "${addr}"
      "${contract}"
      --chain "${CHAIN_ID}"
      --rpc-url "${RPC_URL}"
      --verifier blockscout
      --verifier-url "${EXPLORER_BASE%/}/api/"
      --compiler-version "${PALI_SOLC}"
      --num-of-optimizations 200
      --via-ir
      --watch
    )
    if [[ -n "${ctor}" ]]; then
      args+=(--constructor-args "${ctor}")
    fi
    forge "${args[@]}"
  )
}

account_implementation_bytecode="$(forge_inspect_bytecode "src/pali/PaliSmartAccount.sol:PaliSmartAccount")"
account_implementation_ctor="$(abi_encode "constructor(address)" "${ENTRYPOINT_ADDRESS}")"
account_implementation_init_code="${account_implementation_bytecode}${account_implementation_ctor#0x}"
account_implementation_address="$(
  create2_address "$(salt "account-implementation")" "${account_implementation_init_code}"
)"

factory_bytecode="$(forge_inspect_bytecode "src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory")"
factory_ctor="$(abi_encode "constructor(address,address)" "${account_implementation_address}" "${ENTRYPOINT_ADDRESS}")"
factory_init_code="${factory_bytecode}${factory_ctor#0x}"
factory_address="$(create2_address "$(salt "factory")" "${factory_init_code}")"

# The canonical EntryPoint v0.9 is built with the official account-abstraction
# release settings (optimizer runs 1,000,000, via-ir, default metadata), not
# the Pali profile, so it gets a dedicated verification path from the vendored
# v0.9.0 checkout.
verify_entrypoint() {
  if [[ "$(runtime_code "${ENTRYPOINT_ADDRESS}")" == "0x" ]]; then
    echo "missing EntryPoint v0.9 (${ENTRYPOINT_ADDRESS}); skipping verification because no code is deployed" >&2
    return
  fi

  if is_verified "${ENTRYPOINT_ADDRESS}"; then
    echo "skip   EntryPoint v0.9 (${ENTRYPOINT_ADDRESS}) already verified"
    return
  fi

  echo "submit EntryPoint v0.9 (${ENTRYPOINT_ADDRESS})"
  (
    cd "${AA_DIR}"
    env -u FOUNDRY_BYTECODE_HASH -u FOUNDRY_CBOR_METADATA \
      forge verify-contract \
      "${ENTRYPOINT_ADDRESS}" \
      contracts/core/EntryPoint.sol:EntryPoint \
      --chain "${CHAIN_ID}" \
      --rpc-url "${RPC_URL}" \
      --verifier blockscout \
      --verifier-url "${EXPLORER_BASE%/}/api/" \
      --compiler-version "${PALI_SOLC}" \
      --num-of-optimizations 1000000 \
      --via-ir \
      --watch
  )
}

echo "Verifying zkSYS Pali stack"
echo "  explorer:       ${EXPLORER_BASE}"
echo "  rpc:            ${RPC_URL}"
echo "  chain:          ${CHAIN_ID}"
echo "  entrypoint:     ${ENTRYPOINT_ADDRESS}"
echo "  account impl:   ${account_implementation_address}"
echo "  factory:        ${factory_address}"
echo

verify_entrypoint
verify_contract "${account_implementation_address}" "Pali smart account implementation" "src/pali/PaliSmartAccount.sol:PaliSmartAccount" "${account_implementation_ctor}"
verify_contract "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad" "ECDSA validator module" "src/pali/PaliECDSAValidatorModule.sol:PaliECDSAValidatorModule"
verify_contract "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd" "P-256 passkey validator module" "src/pali/PaliP256WebAuthnValidatorModule.sol:PaliP256WebAuthnValidatorModule"
verify_contract "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a" "SLH-DSA verifier" "src/pali/SLHDSASHA212824Verifier.sol:SLHDSASHA212824Verifier"
verify_contract "0x3fe7586e106eb90988dc2385a5987b7040da06f3" "SLH-DSA validator module" "src/pali/PaliSLHDSAValidatorModule.sol:PaliSLHDSAValidatorModule" "${SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS}"
verify_contract "0xb455eb25bcab13f003a0db5dec5e195ab634afda" "Composite validator module" "src/pali/PaliCompositeValidatorModule.sol:PaliCompositeValidatorModule"
verify_contract "0x6b4e0a92e1cee54b93ede57f7b839a423960b913" "Guardian recovery module" "src/pali/PaliGuardianRecoveryModule.sol:PaliGuardianRecoveryModule"
verify_contract "${factory_address}" "Pali smart account factory" "src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory" "${factory_ctor}"

echo "Waiting for verification results..."
for _ in $(seq 1 30); do
  pending=0
  for addr in \
    "${ENTRYPOINT_ADDRESS}" \
    "${account_implementation_address}" \
    "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad" \
    "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd" \
    "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a" \
    "0x3fe7586e106eb90988dc2385a5987b7040da06f3" \
    "0xb455eb25bcab13f003a0db5dec5e195ab634afda" \
    "0x6b4e0a92e1cee54b93ede57f7b839a423960b913" \
    "${factory_address}"; do
    if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
      continue
    fi
    if ! is_verified "${addr}"; then
      pending=$((pending + 1))
    fi
  done
  if [[ "${pending}" -eq 0 ]]; then
    break
  fi
  sleep 5
done

echo
echo "Final status:"
unverified=0
CONTRACTS=(
  "${ENTRYPOINT_ADDRESS}|EntryPoint v0.9"
  "${account_implementation_address}|Pali smart account implementation"
  "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad|ECDSA validator module"
  "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd|P-256 passkey validator module"
  "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a|SLH-DSA verifier"
  "0x3fe7586e106eb90988dc2385a5987b7040da06f3|SLH-DSA validator module"
  "0xb455eb25bcab13f003a0db5dec5e195ab634afda|Composite validator module"
  "0x6b4e0a92e1cee54b93ede57f7b839a423960b913|Guardian recovery module"
  "${factory_address}|Pali smart account factory"
)
for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label <<< "${entry}"
  if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
    echo "  MISSING    ${label} (${addr})"
    unverified=$((unverified + 1))
    continue
  fi
  if is_verified "${addr}"; then
    echo "  VERIFIED   ${label} (${addr})"
  else
    echo "  UNVERIFIED ${label} (${addr})"
    unverified=$((unverified + 1))
  fi
done

if [[ "${unverified}" -gt 0 ]]; then
  echo "error: ${unverified} contract(s) still unverified after the polling window" >&2
  exit 1
fi
