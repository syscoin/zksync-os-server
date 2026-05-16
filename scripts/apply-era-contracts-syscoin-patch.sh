#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "Usage: $0 /absolute/path/to/era-contracts [--zkstack-only]" >&2
  exit 1
fi

CONTRACTS_PATH="$1"
MODE="${2:-all}"
if [[ "${MODE}" != "all" && "${MODE}" != "--zkstack-only" ]]; then
  echo "Usage: $0 /absolute/path/to/era-contracts [--zkstack-only]" >&2
  exit 1
fi
SUPERPROJECT_PATH="$(git -C "${CONTRACTS_PATH}" rev-parse --show-superproject-working-tree 2>/dev/null || true)"
ERA_ROOT="${SUPERPROJECT_PATH:-${CONTRACTS_PATH}}"
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
  grep -q "Gateway supports BLOBS_ZKSYNC_OS via compact Bitcoin DA refs" "${ERA_ROOT}/zkstack_cli/crates/zkstack/src/commands/chain/gateway/migrate_to_gateway_calldata.rs"
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

check_syscoin_verifier_version_pin() {
  if syscoin_verifier_version_pinned; then
    return 0
  fi

  python3 - "$(deploy_ctm_path)" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
old = "uint32 constant DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 6;"
new = "uint32 constant DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 7;"
if new in text:
    raise SystemExit(0)
if old not in text:
    raise SystemExit("unable to update DEFAULT_ZKSYNC_OS_VERIFIER_VERSION to 7")
PY
}

check_base_contracts_patch() {
  git -C "${CONTRACTS_PATH}" apply --check --recount --exclude='zkstack_cli/**' "${PATCH_FILE}"
}

apply_base_contracts_patch() {
  git -C "${CONTRACTS_PATH}" apply --recount --exclude='zkstack_cli/**' "${PATCH_FILE}"
}

check_base_zkstack_patch() {
  if [[ ! -d "${ERA_ROOT}/zkstack_cli" ]]; then
    echo "error: ${ERA_ROOT}/zkstack_cli not found; cannot apply zkstack_cli patch hunks" >&2
    exit 1
  fi
  git -C "${ERA_ROOT}" apply --check --recount --include='zkstack_cli/**' "${PATCH_FILE}"
}

apply_base_zkstack_patch() {
  git -C "${ERA_ROOT}" apply --recount --include='zkstack_cli/**' "${PATCH_FILE}"
}

ensure_contracts_clean_for_base_patch() {
  if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain)" ]]; then
    echo "error: ${CONTRACTS_PATH} has uncommitted changes and the base contracts patch is not applied" >&2
    git -C "${CONTRACTS_PATH}" status --porcelain >&2
    exit 1
  fi
}

ensure_zkstack_clean_for_patch() {
  if [[ -n "$(git -C "${ERA_ROOT}" status --porcelain -- zkstack_cli)" ]]; then
    echo "error: ${ERA_ROOT}/zkstack_cli has uncommitted changes and the Gateway migration patch is not applied" >&2
    git -C "${ERA_ROOT}" status --porcelain -- zkstack_cli >&2
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

if [[ "${MODE}" == "--zkstack-only" ]]; then
  if syscoin_gateway_da_migration_patched; then
    echo "zkstack Gateway migration patch appears already applied; skipping."
    exit 0
  fi

  ensure_zkstack_clean_for_patch
  echo "Checking zkstack Gateway migration patch applicability..."
  check_base_zkstack_patch

  echo "Applying zkstack Gateway migration patch..."
  apply_base_zkstack_patch
  echo "Patch applied successfully."
  exit 0
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

initial_contracts_status="$(git -C "${CONTRACTS_PATH}" status --porcelain)"
changed=false
contracts_changed=false
need_base_contracts_patch=false
need_verifier_pin=false
need_zkstack_patch=false
need_da_limits_patch=false

if ! base_patch_core_applied; then
  need_base_contracts_patch=true
fi
if ! syscoin_verifier_version_pinned; then
  need_verifier_pin=true
fi
if ! syscoin_gateway_da_migration_patched; then
  need_zkstack_patch=true
fi
if ! da_limits_patch_applied; then
  need_da_limits_patch=true
fi

if [[ "${need_base_contracts_patch}" == true ]]; then
  ensure_contracts_clean_for_base_patch
  echo "Checking base era-contracts Syscoin patch applicability..."
  check_base_contracts_patch
fi

if [[ "${need_verifier_pin}" == true ]]; then
  ensure_contracts_were_clean_for_partial_patch
  check_syscoin_verifier_version_pin
fi

if [[ "${need_zkstack_patch}" == true ]]; then
  ensure_zkstack_clean_for_patch
  echo "Checking zkstack Gateway migration patch applicability..."
  check_base_zkstack_patch
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

if [[ "${need_verifier_pin}" == true ]]; then
  pin_syscoin_verifier_version
  changed=true
  contracts_changed=true
fi

if [[ "${need_zkstack_patch}" == true ]]; then
  echo "Applying zkstack Gateway migration patch..."
  apply_base_zkstack_patch
  changed=true
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
