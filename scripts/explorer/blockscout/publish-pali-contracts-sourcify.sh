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
# metadata-free compilation units that produced the deployed bytecode) via Sourcify's v2 API:
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
  "0xf9cd389c3a980633fb75e9997d463923239aedc9|Smart account implementation|pali-contracts.json|${PALI_SOLC}|src/pali/PaliSmartAccount.sol:PaliSmartAccount"
  "0xa891d5b9bf6ed7c05bfc29c284aa6d4f672118ad|ECDSA validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliECDSAValidatorModule.sol:PaliECDSAValidatorModule"
  "0x3eb5235eba1afa59500c2da1d4c66284aafbf3fd|P-256 passkey validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliP256WebAuthnValidatorModule.sol:PaliP256WebAuthnValidatorModule"
  "0x789d5ac3a14b543a46fc402eedcf31d8c8b93d4a|SLH-DSA verifier|slh-dsa-contracts.json|${SLH_DSA_SOLC}|src/pali/SLHDSASHA212824Verifier.sol:SLHDSASHA212824Verifier"
  "0x3fe7586e106eb90988dc2385a5987b7040da06f3|SLH-DSA validator module|slh-dsa-contracts.json|${SLH_DSA_SOLC}|src/pali/PaliSLHDSAValidatorModule.sol:PaliSLHDSAValidatorModule"
  "0xb455eb25bcab13f003a0db5dec5e195ab634afda|Composite validator module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliCompositeValidatorModule.sol:PaliCompositeValidatorModule"
  "0x6b4e0a92e1cee54b93ede57f7b839a423960b913|Guardian recovery module|pali-contracts.json|${PALI_SOLC}|src/pali/PaliGuardianRecoveryModule.sol:PaliGuardianRecoveryModule"
  "0xa53b1341fc26a81722dd01915346d141f8a0be83|Smart account factory|pali-contracts.json|${PALI_SOLC}|src/pali/PaliSmartAccountFactory.sol:PaliSmartAccountFactory"
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
