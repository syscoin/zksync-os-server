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

deploy_ctm_path() {
  if [[ -f "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" ]]; then
    printf '%s\n' "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol"
  else
    printf '%s\n' "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/DeployCTM.s.sol"
  fi
}

base_patch_core_applied() {
  grep -q "error BitcoinDAPrecompileCallFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "error BitcoinDAVerificationFailed();" "${CONTRACTS_PATH}/da-contracts/contracts/DAContractsErrors.sol" \
  && grep -q "function _verifyBitcoinDA(bytes32 _dataHash) internal view" "${CONTRACTS_PATH}/da-contracts/contracts/BlobsL1DAValidatorZKsyncOS.sol" \
  && grep -q "0xdf7da82999fa7201551447c726fe17d75ce2354a55b971c2bdffcc164d878e5e" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol" \
  && grep -q "create2FactoryAddr != address(0) && create2FactoryAddr.code.length == 0" "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/ecosystem/DeployL1CoreContracts.s.sol" \
  && grep -q "return (create2FactoryAddr, create2FactorySalt);" "${CONTRACTS_PATH}/l1-contracts/deploy-scripts/utils/deploy/Create2FactoryUtils.s.sol"
}

syscoin_verifier_version_pinned() {
  grep -q "DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 7" "$(deploy_ctm_path)"
}

syscoin_gateway_da_migration_patched() {
  grep -q "Gateway supports BLOBS_ZKSYNC_OS via compact Bitcoin DA refs" "${CONTRACTS_PATH}/zkstack_cli/crates/zkstack/src/commands/chain/gateway/migrate_to_gateway_calldata.rs"
}

base_patch_applied() {
  base_patch_core_applied && syscoin_verifier_version_pinned && syscoin_gateway_da_migration_patched
}

da_limits_patch_applied() {
  grep -q "uint256 constant BLOB_SIZE_BYTES = 2 \* 1024 \* 1024;" "${CONTRACTS_PATH}/da-contracts/contracts/CalldataDA.sol" \
  && grep -q "uint256 constant BLOB_SIZE_BYTES = 2 \* 1024 \* 1024;" "${CONTRACTS_PATH}/da-contracts/contracts/DAUtils.sol" \
  && grep -q "uint256 constant MAX_NUMBER_OF_BLOBS = 32;" "${CONTRACTS_PATH}/system-contracts/contracts/Constants.sol" \
  && grep -q "uint256 constant TOTAL_BLOBS_IN_COMMITMENT = 32;" "${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/chain-interfaces/IExecutor.sol"
}

pin_syscoin_verifier_version() {
  if syscoin_verifier_version_pinned; then
    return 0
  fi

  echo "Updating already-applied era-contracts Syscoin patch to register the V7 verifier slot..."
  python3 - "$(deploy_ctm_path)" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
old = "uint32 constant DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 6;"
new = (
    "// SYSCOIN: fresh v31+ deployments use the patched V7 ZKsync OS verification key.\n"
    "uint32 constant DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 7;"
)
if new in text:
    raise SystemExit(0)
if old not in text:
    raise SystemExit("unable to update DEFAULT_ZKSYNC_OS_VERIFIER_VERSION to 7")
path.write_text(text.replace(old, new, 1), encoding="utf-8")
PY
}

# Marker-based idempotency check: if these patch-introduced strings exist, skip.
if base_patch_applied && da_limits_patch_applied; then
  echo "era-contracts syscoin patch appears already applied; skipping."
  exit 0
fi

if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain)" ]]; then
  echo "error: ${CONTRACTS_PATH} has uncommitted changes; aborting patch apply" >&2
  git -C "${CONTRACTS_PATH}" status --porcelain >&2
  exit 1
fi

if base_patch_core_applied && ! syscoin_verifier_version_pinned; then
  pin_syscoin_verifier_version
fi

if base_patch_applied && da_limits_patch_applied; then
  echo "era-contracts syscoin patch appears already applied; skipping."
  exit 0
fi

if base_patch_applied && ! da_limits_patch_applied; then
  echo "Checking Syscoin DA limits patch applicability..."
  git -C "${CONTRACTS_PATH}" apply --check --recount "${DA_LIMITS_PATCH_FILE}"

  echo "Applying era-contracts Syscoin DA limits patch..."
  git -C "${CONTRACTS_PATH}" apply --recount "${DA_LIMITS_PATCH_FILE}"
  echo "Patch applied successfully."
  exit 0
fi

if ! base_patch_applied && da_limits_patch_applied; then
  echo "Checking base era-contracts Syscoin patch applicability..."
  git -C "${CONTRACTS_PATH}" apply --check --recount "${PATCH_FILE}"

  echo "Applying base era-contracts Syscoin patch..."
  git -C "${CONTRACTS_PATH}" apply --recount "${PATCH_FILE}"
  pin_syscoin_verifier_version
  echo "Patch applied successfully."
  exit 0
fi

echo "Checking patch applicability..."
git -C "${CONTRACTS_PATH}" apply --check --recount "${PATCH_FILE}"
git -C "${CONTRACTS_PATH}" apply --check --recount "${DA_LIMITS_PATCH_FILE}"

echo "Applying era-contracts syscoin patch..."
git -C "${CONTRACTS_PATH}" apply --recount "${PATCH_FILE}"
pin_syscoin_verifier_version
git -C "${CONTRACTS_PATH}" apply --recount "${DA_LIMITS_PATCH_FILE}"

echo "Patch applied successfully."
