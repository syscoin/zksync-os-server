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

PLONK_CONTRACT_REL="l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol"
PLONK_KEY_REL="tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json"
FFLONK_CONTRACT_REL="l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol"
FFLONK_KEY_REL="tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json"

patch_forward_applicable() {
  git -C "${CONTRACTS_PATH}" apply --check --recount "$1" >/dev/null 2>&1
}

patch_reverse_applicable() {
  git -C "${CONTRACTS_PATH}" apply --reverse --check --recount "$1" >/dev/null 2>&1
}

verify_exact_file() {
  local relative_path="$1" expected_size="$2" expected_sha256="$3"
  local path="${CONTRACTS_PATH}/${relative_path}" actual_size actual_sha256
  if [[ ! -f "${path}" || -L "${path}" ]]; then
    echo "error: attested Era artifact is not a regular non-symlink file: ${path}" >&2
    exit 1
  fi
  actual_size="$(wc -c < "${path}" | tr -d '[:space:]')"
  if [[ "${actual_size}" != "${expected_size}" ]]; then
    echo "error: Era artifact size mismatch for ${relative_path}: expected=${expected_size} actual=${actual_size}" >&2
    exit 1
  fi
  actual_sha256="$(sha256sum "${path}" | awk '{print $1}')"
  if [[ "${actual_sha256}" != "${expected_sha256}" ]]; then
    echo "error: Era artifact SHA-256 mismatch for ${relative_path}: expected=${expected_sha256} actual=${actual_sha256}" >&2
    exit 1
  fi
}

verify_verifier_artifacts() {
  verify_exact_file "${PLONK_CONTRACT_REL}" 95217 \
    6302e7132a53c1895bf6ee9ede83a2c4e7bdddc5eedbffaabbe69fb043ee7e2f
  verify_exact_file "${PLONK_KEY_REL}" 8082 \
    f2805b9ef334f61c874e152b183035cb1d31172d48c6b125f0e6047c9aaa5168
  verify_exact_file "${FFLONK_CONTRACT_REL}" 77746 \
    9308b1850d4197bd7b6a59cc35029f51b94ffce76f5951848669fd9424a07d48
  verify_exact_file "${FFLONK_KEY_REL}" 1920 \
    a1d093cf2bb0f5331c4a6bbf0e40d5f4888cc850324e8b9e406bde6686f07f77
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

base_forward=false
base_reverse=false
da_forward=false
da_reverse=false
patch_forward_applicable "${PATCH_FILE}" && base_forward=true
patch_reverse_applicable "${PATCH_FILE}" && base_reverse=true
patch_forward_applicable "${DA_LIMITS_PATCH_FILE}" && da_forward=true
patch_reverse_applicable "${DA_LIMITS_PATCH_FILE}" && da_reverse=true

if [[ "${base_reverse}" == true && "${da_reverse}" == true ]]; then
  verify_verifier_artifacts
  echo "Era-contracts Syscoin patches and verifier artifacts are already exact; skipping."
  exit 0
fi

if [[ "${base_forward}" != true || "${da_forward}" != true ]]; then
  echo "error: Era-contracts patch state is partial or diverged" >&2
  echo "error: base(forward=${base_forward}, reverse=${base_reverse}) da-limits(forward=${da_forward}, reverse=${da_reverse})" >&2
  exit 1
fi

if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain)" ]]; then
  echo "error: ${CONTRACTS_PATH} has uncommitted changes before patch application" >&2
  git -C "${CONTRACTS_PATH}" status --porcelain >&2
  exit 1
fi

echo "Applying exact base Era-contracts Syscoin patch..."
git -C "${CONTRACTS_PATH}" apply --recount "${PATCH_FILE}"
echo "Applying exact Era-contracts Syscoin DA limits patch..."
git -C "${CONTRACTS_PATH}" apply --recount "${DA_LIMITS_PATCH_FILE}"

patch_reverse_applicable "${PATCH_FILE}" || {
  echo "error: base Era-contracts patch postimage failed reverse applicability" >&2
  exit 1
}
patch_reverse_applicable "${DA_LIMITS_PATCH_FILE}" || {
  echo "error: Era-contracts DA limits patch postimage failed reverse applicability" >&2
  exit 1
}
verify_verifier_artifacts

echo "Patches applied and verifier artifacts attested successfully."
