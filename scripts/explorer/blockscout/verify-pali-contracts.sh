#!/usr/bin/env bash
# Verify the Pali ERC-4337 smart-account contracts on a Blockscout instance.
#
# This is reproducible and idempotent: it submits Solidity standard-JSON input
# (the exact metadata-free compilation unit that produced the deployed bytecode)
# for every canonical Pali contract, then polls until each reports verified.
#
# Prerequisites on the target Blockscout instance (already wired in
# docker-compose.yml): the smart-contract-verifier microservice must be running
# and the backend must have MICROSERVICE_SC_VERIFIER_{ENABLED,URL,TYPE} set.
#
# The standard-JSON inputs live next to this script under
# pali-verification/standard-json/ and are committed so verification can be
# re-run without the original build server:
#   - pali-contracts.json : 6 Pali contracts (solc 0.8.28, 200 runs, viaIR,
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
FACTORY_CONSTRUCTOR_ARGS="0x000000000000000000000000f9cd389c3a980633fb75e9997d463923239aedc9"
# SLH-DSA validator constructor args: abi.encode(verifier).
SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS="0x000000000000000000000000789d5ac3a14b543a46fc402eedcf31d8c8b93d4a"

# address|label|standard-json|compiler|constructor_args
CONTRACTS=(
  "0x433709009B8330FDa32311DF1C2AFA402eD8D009|EntryPoint v0.9|entrypoint.json|${ENTRYPOINT_SOLC}|"
  "0xf9cd389c3a980633fb75e9997d463923239aedc9|Smart account implementation|pali-contracts.json|${PALI_SOLC}|"
  "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad|ECDSA validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x5e480b89c2089437ebe8f0ff6356d9ba2f07a53a|P-256 passkey validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a|SLH-DSA verifier|slh-dsa-contracts.json|${SLH_DSA_SOLC}|"
  "0x0876d0c5a57bf31c1758c58d2c213fa47b8708db|SLH-DSA validator module|slh-dsa-contracts.json|${SLH_DSA_SOLC}|${SLH_DSA_VALIDATOR_CONSTRUCTOR_ARGS}"
  "0xb455eb25bcab13f003a0db5dec5e195ab634afda|Composite validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x6b4e0a92e1cee54b93ede57f7b839a423960b913|Guardian recovery module|pali-contracts.json|${PALI_SOLC}|"
  "0xdb062bd34ed9b3b7c7a97fca895dd2ff59512370|Smart account factory|pali-contracts.json|${PALI_SOLC}|${FACTORY_CONSTRUCTOR_ARGS}"
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
