#!/usr/bin/env bash
# Verify the Pali ERC-4337 smart-account contracts on a Blockscout instance.
#
# This is reproducible and idempotent: it submits Solidity standard-JSON input
# (the exact compilation unit that produced the deployed bytecode) for every
# canonical Pali contract, then polls until each reports verified.
#
# Prerequisites on the target Blockscout instance (already wired in
# docker-compose.yml): the smart-contract-verifier microservice must be running
# and the backend must have MICROSERVICE_SC_VERIFIER_{ENABLED,URL,TYPE} set.
#
# The standard-JSON inputs live next to this script under
# pali-verification/standard-json/ and are committed so verification can be
# re-run without the original build server:
#   - pali-contracts.json : 6 Pali contracts (solc 0.8.26, 200 runs, viaIR,
#                           cancun). The verifier selects the matching contract
#                           per address by bytecode.
#   - slh-dsa-contracts.json : SLH-DSA verifier and validator (solc 0.8.28,
#                              200 runs, viaIR, cancun).
#   - entrypoint.json     : ERC-4337 EntryPoint v0.9 (solc 0.8.28, 1,000,000
#                           runs, viaIR, cancun).
#
# Canonical addresses are derived in
# pali-wallet/source/utils/smartAccount/deployment.ts (CREATE2 via the
# 0x4e59...4956C deployer with the PALI_SMART_ACCOUNT_ERC7579_V1 salts). Keep
# the addresses below in sync if the registry bytecode/version is bumped.
#
# Usage:
#   ./verify-pali-contracts.sh [EXPLORER_BASE_URL]
# Defaults to the zkTanenbaum public explorer.

set -euo pipefail

EXPLORER_BASE="${1:-https://explorer-zk.tanenbaum.io}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JSON_DIR="${SCRIPT_DIR}/pali-verification/standard-json"

PALI_SOLC="v0.8.28+commit.7893614a"
SLH_DSA_SOLC="v0.8.28+commit.7893614a"
ENTRYPOINT_SOLC="v0.8.28+commit.7893614a"

# Factory constructor args: abi.encode(accountImplementation).
FACTORY_CONSTRUCTOR_ARGS="0x00000000000000000000000016f8c2aa6532929383e34d3c4d1c26aad1f93ae7"
# SLH-DSA validator constructor args: abi.encode(verifier).
SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS="0x000000000000000000000000e34bba0c18b56ec29bbad1370458417c6c3c5176"

# address|label|standard-json|compiler|constructor_args
CONTRACTS=(
  "0x433709009B8330FDa32311DF1C2AFA402eD8D009|EntryPoint v0.9|entrypoint.json|${ENTRYPOINT_SOLC}|"
  "0x16f8c2aa6532929383e34d3c4d1c26aad1f93ae7|Smart account implementation|pali-contracts.json|${PALI_SOLC}|"
  "0x3b5102122e368b7a643e8d55d56d2face1299b34|ECDSA validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x6b802a0db05616768f233d4264edf8cccfd5443c|P-256 passkey validator module|pali-contracts.json|${PALI_SOLC}|"
  "0xe34bba0c18b56ec29bbad1370458417c6c3c5176|SLH-DSA verifier|slh-dsa-contracts.json|${SLH_DSA_SOLC}|"
  "0x684682edf65b9d91d559b70d503558c1ce4be1a2|SLH-DSA validator module|slh-dsa-contracts.json|${SLH_DSA_SOLC}|${SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS}"
  "0xa343139fc7d2397ee000d40b26a2598ba4ffd3e3|Composite validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x0c2afbdb0cbf5f8a9dad12f1937eb68ccb7ecf9e|Guardian recovery module|pali-contracts.json|${PALI_SOLC}|"
  "0xa4279b355923cfbdbb0bd2cc481c944c715db3ca|Smart account factory|pali-contracts.json|${PALI_SOLC}|${FACTORY_CONSTRUCTOR_ARGS}"
)

is_verified() {
  curl -s "${EXPLORER_BASE}/api/v2/smart-contracts/$1" \
    | grep -q '"is_verified":true'
}

for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label json compiler ctor <<< "${entry}"

  if is_verified "${addr}"; then
    echo "skip   ${label} (${addr}) already verified"
    continue
  fi

  # Blockscout only accepts verification once it knows the address is a
  # contract; a single address-page request triggers the on-demand code fetch
  # for CREATE2 deployments whose creation tx was not indexed.
  curl -s -o /dev/null "${EXPLORER_BASE}/api/v2/addresses/${addr}"

  args=(-s -X POST
    "${EXPLORER_BASE}/api/v2/smart-contracts/${addr}/verification/via/standard-input"
    -F "compiler_version=${compiler}"
    -F "license_type=mit"
    -F "files[0]=@${JSON_DIR}/${json};type=application/json")
  if [[ -n "${ctor}" ]]; then
    args+=(-F "constructor_args=${ctor}")
  fi

  echo "submit ${label} (${addr})"
  curl "${args[@]}"
  echo
done

echo "Waiting for verification results..."
for _ in $(seq 1 30); do
  pending=0
  for entry in "${CONTRACTS[@]}"; do
    addr="${entry%%|*}"
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
for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label _ _ _ <<< "${entry}"
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
