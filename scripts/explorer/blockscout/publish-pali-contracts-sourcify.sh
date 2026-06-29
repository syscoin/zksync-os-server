#!/usr/bin/env bash
# Publish the zkSYS Pali ERC-4337 contracts to Sourcify.
#
# Sourcify is the durable, explorer-independent verification store: Blockscout
# keeps its verification results in its own postgres, so a DB reset would lose
# them, while a Sourcify match survives and is re-imported automatically by
# any Blockscout with SOURCIFY_INTEGRATION_ENABLED=true (the backend queries
# Sourcify when an unverified contract page is opened).
#
# Idempotent: contracts that Sourcify already has a match for are skipped.
#
# Usage:
#   PAYMASTER_ADDRESS=0x... PAYMASTER_OWNER=0x... ./publish-pali-contracts-sourcify.sh [SOURCIFY_SERVER] [CHAIN_ID]
# Defaults to the public Sourcify server and zkTanenbaum (57057).

set -euo pipefail

SOURCIFY_SERVER="${1:-https://sourcify.dev/server}"
CHAIN_ID="${2:-57057}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"

PALI_SOLC="v0.8.28+commit.7893614a"
RPC_URL="${ZKTANENBAUM_RPC_URL:-${RPC_URL:-https://rpc-zk.tanenbaum.io}}"
ENTRYPOINT_ADDRESS="${ENTRYPOINT_ADDRESS:-0x4337724c89B1Df0cA99FE53640123f0444dcE0F3}"
PALI_CREATE2_DEPLOYER_ADDRESS="${PALI_CREATE2_DEPLOYER_ADDRESS:-0x4e59b44847b379578588920cA78FbF26c0B4956C}"
PALI_INFRASTRUCTURE_VERSION="PALI_SMART_ACCOUNT_ERC7579_V1"

: "${PAYMASTER_ADDRESS:?PAYMASTER_ADDRESS is required}"
: "${PAYMASTER_OWNER:?PAYMASTER_OWNER is required and must be the deployment-time paymaster owner}"

# SLH-DSA validator constructor args: abi.encode(verifier).
SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS="0x000000000000000000000000789d5ac3a14b543a46fc402eedcf31d8c8b93d4a31e33d9848db6a8821cf39adeb347aff047a308f52b04aee2a398e29fee8b628"

lower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

forge_inspect_bytecode() {
  (
    cd "${CONTRACTS_DIR}"
    forge inspect "$1" bytecode
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

match_status() {
  # Prints the Sourcify match kind ("exact_match", "match") or "none".
  curl -s "${SOURCIFY_SERVER}/v2/contract/${CHAIN_ID}/$1" \
    | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{const j=JSON.parse(d);process.stdout.write(j.match||"none")}catch{process.stdout.write("none")}})'
}

publish_contract() {
  local addr="$1"
  local label="$2"
  local contract="$3"
  local ctor="${4:-}"

  if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
    echo "missing ${label} (${addr}); skipping Sourcify publish because no code is deployed" >&2
    return
  fi

  if [[ "$(match_status "${addr}")" != "none" ]]; then
    echo "skip   ${label} (${addr}) already on Sourcify"
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
      --verifier sourcify
      --verifier-url "${SOURCIFY_SERVER}"
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

paymaster_entrypoint="$(cast call "${PAYMASTER_ADDRESS}" "entryPoint()(address)" --rpc-url "${RPC_URL}")"
paymaster_token="$(cast call "${PAYMASTER_ADDRESS}" "token()(address)" --rpc-url "${RPC_URL}")"
paymaster_reserve="$(cast call "${PAYMASTER_ADDRESS}" "TARGET_ENTRY_POINT_RESERVE()(uint256)" --rpc-url "${RPC_URL}")"
if [[ "$(lower "${paymaster_entrypoint}")" != "$(lower "${ENTRYPOINT_ADDRESS}")" ]]; then
  echo "error: paymaster entryPoint()=${paymaster_entrypoint}, expected ${ENTRYPOINT_ADDRESS}" >&2
  exit 1
fi
paymaster_ctor="$(
  abi_encode \
    "constructor(address,address,address,uint256)" \
    "${ENTRYPOINT_ADDRESS}" \
    "${paymaster_token}" \
    "${PAYMASTER_OWNER}" \
    "${paymaster_reserve}"
)"

echo "Publishing zkSYS Pali stack to Sourcify"
echo "  sourcify:       ${SOURCIFY_SERVER}"
echo "  rpc:            ${RPC_URL}"
echo "  chain:          ${CHAIN_ID}"
echo "  entrypoint:     ${ENTRYPOINT_ADDRESS}"
echo "  account impl:   ${account_implementation_address}"
echo "  factory:        ${factory_address}"
echo "  paymaster:      ${PAYMASTER_ADDRESS}"
echo "  paymaster token:${paymaster_token}"
echo "  paymaster owner:${PAYMASTER_OWNER}"
echo "  reserve:        ${paymaster_reserve}"
echo

publish_contract "${ENTRYPOINT_ADDRESS}" "SyscoinEntryPoint" "src/pali/SyscoinEntryPoint.sol:SyscoinEntryPoint"
publish_contract "${account_implementation_address}" "Pali smart account implementation" "src/pali/PaliSmartAccount.sol:PaliSmartAccount" "${account_implementation_ctor}"
publish_contract "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad" "ECDSA validator module" "src/pali/PaliECDSAValidatorModule.sol:PaliECDSAValidatorModule"
publish_contract "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd" "P-256 passkey validator module" "src/pali/PaliP256WebAuthnValidatorModule.sol:PaliP256WebAuthnValidatorModule"
publish_contract "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a" "SLH-DSA verifier" "src/pali/SLHDSASHA212824Verifier.sol:SLHDSASHA212824Verifier"
publish_contract "0x3fe7586e106eb90988dc2385a5987b7040da06f3" "SLH-DSA validator module" "src/pali/PaliSLHDSAValidatorModule.sol:PaliSLHDSAValidatorModule" "${SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS}"
publish_contract "0xb455eb25bcab13f003a0db5dec5e195ab634afda" "Composite validator module" "src/pali/PaliCompositeValidatorModule.sol:PaliCompositeValidatorModule"
publish_contract "0x6b4e0a92e1cee54b93ede57f7b839a423960b913" "Guardian recovery module" "src/pali/PaliGuardianRecoveryModule.sol:PaliGuardianRecoveryModule"
publish_contract "${factory_address}" "Pali smart account factory" "src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory" "${factory_ctor}"
publish_contract "${PAYMASTER_ADDRESS}" "Pali fixed-rate token paymaster" "src/pali/PaliFixedRateTokenPaymaster.sol:PaliFixedRateTokenPaymaster" "${paymaster_ctor}"

echo "Waiting for Sourcify verification results..."
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
    "${factory_address}" \
    "${PAYMASTER_ADDRESS}"; do
    if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
      continue
    fi
    if [[ "$(match_status "${addr}")" == "none" ]]; then
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
  "${ENTRYPOINT_ADDRESS}|SyscoinEntryPoint"
  "${account_implementation_address}|Pali smart account implementation"
  "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad|ECDSA validator module"
  "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd|P-256 passkey validator module"
  "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a|SLH-DSA verifier"
  "0x3fe7586e106eb90988dc2385a5987b7040da06f3|SLH-DSA validator module"
  "0xb455eb25bcab13f003a0db5dec5e195ab634afda|Composite validator module"
  "0x6b4e0a92e1cee54b93ede57f7b839a423960b913|Guardian recovery module"
  "${factory_address}|Pali smart account factory"
  "${PAYMASTER_ADDRESS}|Pali fixed-rate token paymaster"
)
for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label <<< "${entry}"
  if [[ "$(runtime_code "${addr}")" == "0x" ]]; then
    echo "  MISSING  ${label} (${addr})"
    unverified=$((unverified + 1))
    continue
  fi
  match="$(match_status "${addr}")"
  if [[ "${match}" != "none" ]]; then
    echo "  ${match}  ${label} (${addr})"
  else
    echo "  UNVERIFIED  ${label} (${addr})"
    unverified=$((unverified + 1))
  fi
done

if [[ "${unverified}" -gt 0 ]]; then
  echo "error: ${unverified} contract(s) not verified on Sourcify" >&2
  exit 1
fi
