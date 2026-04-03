#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/era-contracts" >&2
  exit 1
fi

CONTRACTS_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/era-contracts-syscoin.patch"

if [[ ! -e "${CONTRACTS_PATH}/.git" ]]; then
  echo "error: ${CONTRACTS_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi

# Refresh nested submodule URLs from .gitmodules and update recursively
# after checking out the target era-contracts commit.
NESTED_PATH="lib/@matterlabs/zksync-contracts"
git -C "${CONTRACTS_PATH}" submodule sync --recursive
git -C "${CONTRACTS_PATH}" submodule update --init --recursive

# Enforce exact nested SHA pinned by the checked-out era-contracts commit.
EXPECTED_NESTED_SHA="$(git -C "${CONTRACTS_PATH}" ls-tree HEAD "${NESTED_PATH}" | awk '{print $3}')"
if [[ -z "${EXPECTED_NESTED_SHA}" ]]; then
  echo "error: could not resolve expected nested submodule sha for ${NESTED_PATH}" >&2
  exit 1
fi
ACTUAL_NESTED_SHA="$(git -C "${CONTRACTS_PATH}/${NESTED_PATH}" rev-parse HEAD)"
if [[ "${ACTUAL_NESTED_SHA}" != "${EXPECTED_NESTED_SHA}" ]]; then
  echo "error: nested submodule sha mismatch expected=${EXPECTED_NESTED_SHA} actual=${ACTUAL_NESTED_SHA}" >&2
  exit 1
fi

# Marker-based idempotency check: if these patch-introduced strings exist, skip.
if grep -q "error BitcoinDAPrecompileCallFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "error BitcoinDAVerificationFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "function _verifyBitcoinDA(bytes32 _dataHash) internal view" "${CONTRACTS_PATH}/da-contracts/contracts/BlobsL1DAValidatorZKsyncOS.sol" \
  && grep -q "DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 7;" "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/DeployCTM.s.sol" \
  && grep -q "0x739d5ed5fea55cb873fa1ba8d698a20f3fd0d9d2871228cd397c518c41d80e99" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol" \
  && grep -q "create2FactoryAddr != address(0) && create2FactoryAddr.code.length == 0" "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/ecosystem/DeployL1CoreContracts.s.sol" \
  && grep -q "return (create2FactoryAddr, create2FactorySalt);" "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/utils/deploy/Create2FactoryUtils.s.sol"; then
  echo "era-contracts syscoin patch appears already applied; skipping."
  exit 0
fi

if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain)" ]]; then
  echo "error: ${CONTRACTS_PATH} has uncommitted changes; aborting patch apply" >&2
  git -C "${CONTRACTS_PATH}" status --porcelain >&2
  exit 1
fi

echo "Checking patch applicability..."
git -C "${CONTRACTS_PATH}" apply --check --recount "${PATCH_FILE}"

echo "Applying era-contracts syscoin patch..."
git -C "${CONTRACTS_PATH}" apply --recount "${PATCH_FILE}"

echo "Patch applied successfully."
