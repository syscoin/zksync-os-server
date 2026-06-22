#!/usr/bin/env bash
# Publish the Pali ERC-4337 smart-account contracts to Sourcify.
#
# Sourcify is the durable, explorer-independent verification store: Blockscout
# keeps its verification results in its own postgres, so a DB reset would lose
# them, while a Sourcify match survives and is re-imported automatically by
# any Blockscout with SOURCIFY_INTEGRATION_ENABLED=true (the backend queries
# Sourcify when an unverified contract page is opened).
#
# Reuses the committed standard-JSON inputs from pali-verification/ (the exact
# compilation units that produced the deployed bytecode) via Sourcify's v2 API:
#   POST /v2/verify/{chainId}/{address}  {stdJsonInput, compilerVersion,
#                                         contractIdentifier}
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
JSON_DIR="${SCRIPT_DIR}/pali-verification/standard-json"

# Sourcify takes the long compiler version without the leading "v".
PALI_SOLC="0.8.28+commit.7893614a"
SLH_DSA_SOLC="0.8.28+commit.7893614a"
ENTRYPOINT_SOLC="0.8.28+commit.7893614a"

# address|label|standard-json|compiler|source-path:ContractName
CONTRACTS=(
  "0x433709009B8330FDa32311DF1C2AFA402eD8D009|EntryPoint v0.9|entrypoint.json|${ENTRYPOINT_SOLC}|contracts/core/EntryPoint.sol:EntryPoint"
  "0x16f8c2aa6532929383e34d3c4d1c26aad1f93ae7|Smart account implementation|pali-contracts.json|${PALI_SOLC}|src/pali/PaliSmartAccount.sol:PaliSmartAccount"
  "0x3b5102122e368b7a643e8d55d56d2face1299b34|ECDSA validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliECDSAValidatorModule.sol:PaliECDSAValidatorModule"
  "0x6b802a0db05616768f233d4264edf8cccfd5443c|P-256 passkey validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliP256WebAuthnValidatorModule.sol:PaliP256WebAuthnValidatorModule"
  "0xe34bba0c18b56ec29bbad1370458417c6c3c5176|SLH-DSA verifier|slh-dsa-contracts.json|${SLH_DSA_SOLC}|src/pali/SLHDSASHA212824Verifier.sol:SLHDSASHA212824Verifier"
  "0x684682edf65b9d91d559b70d503558c1ce4be1a2|SLH-DSA validator module|slh-dsa-contracts.json|${SLH_DSA_SOLC}|src/pali/PaliSLHDSAValidatorModule.sol:PaliSLHDSAValidatorModule"
  "0xa343139fc7d2397ee000d40b26a2598ba4ffd3e3|Composite validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliCompositeValidatorModule.sol:PaliCompositeValidatorModule"
  "0x0c2afbdb0cbf5f8a9dad12f1937eb68ccb7ecf9e|Guardian recovery module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliGuardianRecoveryModule.sol:PaliGuardianRecoveryModule"
  "0xa4279b355923cfbdbb0bd2cc481c944c715db3ca|Smart account factory|pali-contracts.json|${PALI_SOLC}|src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory"
)

match_status() {
  # Prints the Sourcify match kind ("exact_match", "match") or "none".
  curl -s "${SOURCIFY_SERVER}/v2/contract/${CHAIN_ID}/$1" \
    | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{const j=JSON.parse(d);process.stdout.write(j.match||"none")}catch{process.stdout.write("none")}})'
}

for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label json compiler identifier <<< "${entry}"

  if [[ "$(match_status "${addr}")" != "none" ]]; then
    echo "skip   ${label} (${addr}) already on Sourcify"
    continue
  fi

  echo "submit ${label} (${addr})"
  body="$(node -e '
    const fs = require("fs");
    const [jsonPath, compilerVersion, contractIdentifier] = process.argv.slice(1);
    process.stdout.write(JSON.stringify({
      stdJsonInput: JSON.parse(fs.readFileSync(jsonPath, "utf8")),
      compilerVersion,
      contractIdentifier,
    }));
  ' "${JSON_DIR}/${json}" "${compiler}" "${identifier}")"

  curl -sS -X POST "${SOURCIFY_SERVER}/v2/verify/${CHAIN_ID}/${addr}" \
    -H 'Content-Type: application/json' \
    --data-raw "${body}"
  echo
done

echo "Waiting for Sourcify verification results..."
for _ in $(seq 1 30); do
  pending=0
  for entry in "${CONTRACTS[@]}"; do
    addr="${entry%%|*}"
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
for entry in "${CONTRACTS[@]}"; do
  IFS='|' read -r addr label _ _ _ <<< "${entry}"
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
