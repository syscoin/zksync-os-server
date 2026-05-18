#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/era-contracts" >&2
  exit 1
fi

CONTRACTS_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/era-contracts-syscoin.patch"
DA_LIMITS_PATCH_FILE="${SCRIPT_DIR}/patches/era-contracts-syscoin-da-limits.patch"

if [[ ! -e "${CONTRACTS_PATH}/.git" ]]; then
  echo "error: ${CONTRACTS_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi
if [[ ! -f "${DA_LIMITS_PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${DA_LIMITS_PATCH_FILE}" >&2
  exit 1
fi

base_patch_core_applied() {
  grep -q "error BitcoinDAPrecompileCallFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "error BitcoinDAVerificationFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "function _verifyBitcoinDA(bytes32 _dataHash) internal view" "${CONTRACTS_PATH}/da-contracts/contracts/BlobsL1DAValidatorZKsyncOS.sol" \
  && grep -q "0xdf7da82999fa7201551447c726fe17d75ce2354a55b971c2bdffcc164d878e5e" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol" \
  && grep -q "L2DACommitmentScheme.BLOBS_ZKSYNC_OS" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol"
}

base_patch_applied() {
  base_patch_core_applied
}

da_limits_patch_applied() {
  grep -q "uint256 constant BLOB_SIZE_BYTES = 2 \* 1024 \* 1024;" "${CONTRACTS_PATH}/da-contracts/contracts/CalldataDA.sol" \
  && grep -q "uint256 constant BLOB_SIZE_BYTES = 2 \* 1024 \* 1024;" "${CONTRACTS_PATH}/da-contracts/contracts/DAUtils.sol" \
  && grep -q "uint256 constant MAX_NUMBER_OF_BLOBS = 32;" "${CONTRACTS_PATH}/system-contracts/contracts/Constants.sol" \
  && grep -q "uint256 constant TOTAL_BLOBS_IN_COMMITMENT = 32;" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/chain-interfaces/IExecutor.sol"
}

check_base_contracts_patch() {
  git -C "${CONTRACTS_PATH}" apply --check --recount "${PATCH_FILE}"
}

apply_base_contracts_patch() {
  git -C "${CONTRACTS_PATH}" apply --recount "${PATCH_FILE}"
}

ensure_contracts_clean_for_base_patch() {
  if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain)" ]]; then
    echo "error: ${CONTRACTS_PATH} has uncommitted changes and the base contracts patch is not applied" >&2
    git -C "${CONTRACTS_PATH}" status --porcelain >&2
    exit 1
  fi
}

ensure_contracts_were_clean_for_partial_patch() {
  if [[ -n "${initial_contracts_status}" && "${contracts_changed}" == false ]]; then
    echo "error: ${CONTRACTS_PATH} has uncommitted changes and a contracts patch component is not applied" >&2
    printf '%s\n' "${initial_contracts_status}" >&2
    exit 1
  fi
}

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

initial_contracts_status="$(git -C "${CONTRACTS_PATH}" status --porcelain)"
changed=false
contracts_changed=false
need_base_contracts_patch=false
need_da_limits_patch=false

if ! base_patch_core_applied; then
  need_base_contracts_patch=true
fi
if ! da_limits_patch_applied; then
  need_da_limits_patch=true
fi

if [[ "${need_base_contracts_patch}" == true ]]; then
  ensure_contracts_clean_for_base_patch
  echo "Checking base era-contracts Syscoin patch applicability..."
  check_base_contracts_patch
fi

if [[ "${need_da_limits_patch}" == true ]]; then
  ensure_contracts_were_clean_for_partial_patch
  echo "Checking Syscoin DA limits patch applicability..."
  git -C "${CONTRACTS_PATH}" apply --check --recount "${DA_LIMITS_PATCH_FILE}"
fi

if [[ "${need_base_contracts_patch}" == true ]]; then
  echo "Applying base era-contracts Syscoin patch..."
  apply_base_contracts_patch
  changed=true
  contracts_changed=true
fi

if [[ "${need_da_limits_patch}" == true ]]; then
  echo "Applying era-contracts Syscoin DA limits patch..."
  git -C "${CONTRACTS_PATH}" apply --recount "${DA_LIMITS_PATCH_FILE}"
  changed=true
  contracts_changed=true
fi

if [[ "${changed}" == false ]]; then
  echo "era-contracts syscoin patch appears already applied; skipping."
  exit 0
fi

echo "Patch applied successfully."
