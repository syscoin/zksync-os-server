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

PALI_SOLC="v0.8.26+commit.8a97fa7a"
ENTRYPOINT_SOLC="v0.8.28+commit.7893614a"

# Factory constructor args: abi.encode(accountImplementation, entryPoint).
FACTORY_CONSTRUCTOR_ARGS="0x000000000000000000000000d8549b9a7ed189947d4cc34be0370b3ee8547b46000000000000000000000000433709009b8330fda32311df1c2afa402ed8d009"

# address|label|standard-json|compiler|constructor_args
CONTRACTS=(
  "0x433709009B8330FDa32311DF1C2AFA402eD8D009|EntryPoint v0.9|entrypoint.json|${ENTRYPOINT_SOLC}|"
  "0xD8549B9a7ED189947D4Cc34Be0370B3eE8547B46|Smart account implementation|pali-contracts.json|${PALI_SOLC}|"
  "0xce2cBf654544db522187c5F4D1446016cF505093|ECDSA validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x3B590190A11119dF42864efaCe0C6E3E0aF02ac8|P-256 passkey validator module|pali-contracts.json|${PALI_SOLC}|"
  "0xCf82A12c0296072C528A5957a67F63842100861A|Composite validator module|pali-contracts.json|${PALI_SOLC}|"
  "0x752dfc110cD2343E06b9eEDEc0B0dC833fB0A2cB|Guardian recovery module|pali-contracts.json|${PALI_SOLC}|"
  "0x1e399Ed1B391cAbC174ef5F708FAb225a22Dc726|Smart account factory|pali-contracts.json|${PALI_SOLC}|${FACTORY_CONSTRUCTOR_ARGS}"
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
for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label _ _ _ <<< "${entry}"
  if is_verified "${addr}"; then
    echo "  VERIFIED   ${label} (${addr})"
  else
    echo "  UNVERIFIED ${label} (${addr})"
  fi
done
