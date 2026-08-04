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
#   ./publish-pali-contracts-sourcify.sh [SOURCIFY_SERVER] [CHAIN_ID]
# Defaults to the public Sourcify server and zkTanenbaum (57057).

set -euo pipefail

SOURCIFY_SERVER="${1:-https://sourcify.dev/server}"
CHAIN_ID="${2:-57057}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"
AA_DIR="${REPO_ROOT}/integration-tests/test-contracts/lib/account-abstraction"

PALI_SOLC="v0.8.28+commit.7893614a"
PALI_SOLC_VERSION="${PALI_SOLC_VERSION:-0.8.28}"
RPC_URL="${ZKTANENBAUM_RPC_URL:-${RPC_URL:-https://rpc-zk.tanenbaum.io}}"
# Canonical ERC-4337 EntryPoint v0.9 singleton.
ENTRYPOINT_ADDRESS="${ENTRYPOINT_ADDRESS:-0x433709009B8330FDa32311DF1C2AFA402eD8D009}"
PALI_CREATE2_DEPLOYER_ADDRESS="${PALI_CREATE2_DEPLOYER_ADDRESS:-0x4e59b44847b379578588920cA78FbF26c0B4956C}"
PALI_INFRASTRUCTURE_VERSION="PALI_SMART_ACCOUNT_ERC7579_V2"

export FOUNDRY_BYTECODE_HASH=none
export FOUNDRY_CBOR_METADATA=false

# SLH-DSA validator constructor args: abi.encode(verifier).
SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS="0x000000000000000000000000588d8afa40c08983a114957310c04d05a9dcb56d31e33d9848db6a8821cf39adeb347aff047a308f52b04aee2a398e29fee8b628"

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

# The canonical EntryPoint v0.9 is built with the official account-abstraction
# release settings (optimizer runs 1,000,000, via-ir, default metadata), not
# the Pali profile, so it gets a dedicated publish path from the vendored
# v0.9.0 checkout.
publish_entrypoint() {
  if [[ "$(runtime_code "${ENTRYPOINT_ADDRESS}")" == "0x" ]]; then
    echo "missing EntryPoint v0.9 (${ENTRYPOINT_ADDRESS}); skipping Sourcify publish because no code is deployed" >&2
    return
  fi

  if [[ "$(match_status "${ENTRYPOINT_ADDRESS}")" != "none" ]]; then
    echo "skip   EntryPoint v0.9 (${ENTRYPOINT_ADDRESS}) already on Sourcify"
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
      --verifier sourcify \
      --verifier-url "${SOURCIFY_SERVER}" \
      --compiler-version "${PALI_SOLC}" \
      --num-of-optimizations 1000000 \
      --via-ir \
      --watch
  )
}

echo "Publishing zkSYS Pali stack to Sourcify"
echo "  sourcify:       ${SOURCIFY_SERVER}"
echo "  rpc:            ${RPC_URL}"
echo "  chain:          ${CHAIN_ID}"
echo "  entrypoint:     ${ENTRYPOINT_ADDRESS}"
echo "  account impl:   ${account_implementation_address}"
echo "  factory:        ${factory_address}"
echo

publish_entrypoint
publish_contract "${account_implementation_address}" "Pali smart account implementation" "src/pali/PaliSmartAccount.sol:PaliSmartAccount" "${account_implementation_ctor}"
publish_contract "0xbe057b217a1e17ffdc27c5262db790f0aaaa9133" "ECDSA validator module" "src/pali/PaliECDSAValidatorModule.sol:PaliECDSAValidatorModule"
publish_contract "0xcde85b38a769dbe696574b5f4d8fa6ff4e420a24" "P-256 passkey validator module" "src/pali/PaliP256WebAuthnValidatorModule.sol:PaliP256WebAuthnValidatorModule"
publish_contract "0x588d8afa40c08983a114957310c04d05a9dcb56d" "SLH-DSA verifier" "src/pali/SLHDSASHA212824Verifier.sol:SLHDSASHA212824Verifier"
publish_contract "0x3b35e207243164753af0b6d2d99c7ad61f4c4034" "SLH-DSA validator module" "src/pali/PaliSLHDSAValidatorModule.sol:PaliSLHDSAValidatorModule" "${SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS}"
publish_contract "0x85b6218f5ef96e8e33bed1b08ba6d021bd574bd9" "Composite validator module" "src/pali/PaliCompositeValidatorModule.sol:PaliCompositeValidatorModule"
publish_contract "0x23f0801ab25feee643253cd1ee5f8962bf3c63db" "Guardian recovery module" "src/pali/PaliGuardianRecoveryModule.sol:PaliGuardianRecoveryModule"
publish_contract "${factory_address}" "Pali smart account factory" "src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory" "${factory_ctor}"

echo "Waiting for Sourcify verification results..."
for _ in $(seq 1 30); do
  pending=0
  for addr in \
    "${ENTRYPOINT_ADDRESS}" \
    "${account_implementation_address}" \
    "0xbe057b217a1e17ffdc27c5262db790f0aaaa9133" \
    "0xcde85b38a769dbe696574b5f4d8fa6ff4e420a24" \
    "0x588d8afa40c08983a114957310c04d05a9dcb56d" \
    "0x3b35e207243164753af0b6d2d99c7ad61f4c4034" \
    "0x85b6218f5ef96e8e33bed1b08ba6d021bd574bd9" \
    "0x23f0801ab25feee643253cd1ee5f8962bf3c63db" \
    "${factory_address}"; do
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
  "${ENTRYPOINT_ADDRESS}|EntryPoint v0.9"
  "${account_implementation_address}|Pali smart account implementation"
  "0xbe057b217a1e17ffdc27c5262db790f0aaaa9133|ECDSA validator module"
  "0xcde85b38a769dbe696574b5f4d8fa6ff4e420a24|P-256 passkey validator module"
  "0x588d8afa40c08983a114957310c04d05a9dcb56d|SLH-DSA verifier"
  "0x3b35e207243164753af0b6d2d99c7ad61f4c4034|SLH-DSA validator module"
  "0x85b6218f5ef96e8e33bed1b08ba6d021bd574bd9|Composite validator module"
  "0x23f0801ab25feee643253cd1ee5f8962bf3c63db|Guardian recovery module"
  "${factory_address}|Pali smart account factory"
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
