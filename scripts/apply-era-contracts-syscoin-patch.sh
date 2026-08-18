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
EXPECTED_CONTRACTS_HEAD="fdb60be1a49f5f0a371fc24b747cf5bdea1b1f74"
EXPECTED_BASE_PATCH_SIZE="57182"
EXPECTED_BASE_PATCH_SHA256="3a9b5ba568b5f0e999726b60d42c2d646f4e51e9240625db061d49f2da2b11d0"
EXPECTED_DA_LIMITS_PATCH_SIZE="3237"
EXPECTED_DA_LIMITS_PATCH_SHA256="db9018ab4e7091bbe74413b7d59ccc900ae766efecffe91e96891ab378213a3b"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

file_size() {
  wc -c < "$1" | tr -d '[:space:]'
}

if [[ ! -e "${CONTRACTS_PATH}/.git" ]]; then
  echo "error: ${CONTRACTS_PATH} is not a git repository root" >&2
  exit 1
fi

# SYSCOIN: the generated V7 verifier and patch contexts are reviewed against
# one exact era-contracts tree. A moved branch or adjacent release must fail
# before submodule setup or any working-tree mutation.
ACTUAL_CONTRACTS_HEAD="$(git -C "${CONTRACTS_PATH}" rev-parse HEAD)"
if [[ "${ACTUAL_CONTRACTS_HEAD}" != "${EXPECTED_CONTRACTS_HEAD}" ]]; then
  echo "error: era-contracts HEAD ${ACTUAL_CONTRACTS_HEAD} != ${EXPECTED_CONTRACTS_HEAD}" >&2
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

if [[ "$(file_size "${PATCH_FILE}")" != "${EXPECTED_BASE_PATCH_SIZE}" \
  || "$(sha256_file "${PATCH_FILE}")" != "${EXPECTED_BASE_PATCH_SHA256}" ]]; then
  echo "error: base era-contracts patch size or SHA-256 differs from the reviewed artifact" >&2
  exit 1
fi
if [[ "$(file_size "${DA_LIMITS_PATCH_FILE}")" != "${EXPECTED_DA_LIMITS_PATCH_SIZE}" \
  || "$(sha256_file "${DA_LIMITS_PATCH_FILE}")" != "${EXPECTED_DA_LIMITS_PATCH_SHA256}" ]]; then
  echo "error: era-contracts DA-limits patch size or SHA-256 differs from the reviewed artifact" >&2
  exit 1
fi

base_patch_core_applied() {
  git -C "${CONTRACTS_PATH}" apply --reverse --check --recount "${PATCH_FILE}" \
    >/dev/null 2>&1
}

base_patch_applied() {
  base_patch_core_applied
}

da_limits_patch_applied() {
  git -C "${CONTRACTS_PATH}" apply --reverse --check --recount "${DA_LIMITS_PATCH_FILE}" \
    >/dev/null 2>&1
}

verify_canonical_verifier_artifacts() {
  local plonk_contract plonk_key fflonk_contract fflonk_key
  plonk_contract="${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol"
  plonk_key="${CONTRACTS_PATH}/tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json"
  fflonk_contract="${CONTRACTS_PATH}/l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol"
  fflonk_key="${CONTRACTS_PATH}/tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json"

  # SYSCOIN: bind the generated contract to the exact twice-reproduced CPU VK;
  # Fflonk is unrelated to the V7 replacement and must remain byte-identical.
  [[ "$(file_size "${plonk_contract}")" == "95217" \
    && "$(sha256_file "${plonk_contract}")" == "6302e7132a53c1895bf6ee9ede83a2c4e7bdddc5eedbffaabbe69fb043ee7e2f" \
    && "$(file_size "${plonk_key}")" == "8082" \
    && "$(sha256_file "${plonk_key}")" == "f2805b9ef334f61c874e152b183035cb1d31172d48c6b125f0e6047c9aaa5168" \
    && "$(file_size "${fflonk_contract}")" == "77746" \
    && "$(sha256_file "${fflonk_contract}")" == "9308b1850d4197bd7b6a59cc35029f51b94ffce76f5951848669fd9424a07d48" \
    && "$(file_size "${fflonk_key}")" == "1920" \
    && "$(sha256_file "${fflonk_key}")" == "a1d093cf2bb0f5331c4a6bbf0e40d5f4888cc850324e8b9e406bde6686f07f77" ]] \
    || { echo "error: generated ZKsyncOS verifier/key artifacts differ from the reviewed V7 set" >&2; exit 1; }

  [[ "$(grep -c '0x54bcb6abdcb4c8d8e088cc9f2ea9cc3505a8187a45b69e19e830590df6c9b0df' "${plonk_contract}")" == "1" ]] \
    || { echo "error: replacement V7 VK hash is not present exactly once in the Plonk verifier" >&2; exit 1; }
  if grep -Eq '0x(36fe99d4150fe05ea8eae636b5addaf34f5cc0c7764c03deacdd50f064aa1026|6f837bbef255ebde36677f3accb456e16253fe43f4091b0e820bff0cf95a32a0|7461d51489c4cdb20ffae885de79ca0424e9c82f4d9ed6dbed998b379b0ba1a1|23156cf220288cd1e436dccfc09aa4883ea8288da61aa69e2c7251b0c0c44ccd)' "${plonk_contract}"; then
    echo "error: stale historical/upstream V7 VK hash remains in the Plonk verifier" >&2
    exit 1
  fi
}

verify_only_expected_changes() {
  local expected_paths actual_paths
  expected_paths="$({
    git -C "${CONTRACTS_PATH}" apply --numstat --recount "${PATCH_FILE}"
    git -C "${CONTRACTS_PATH}" apply --numstat --recount "${DA_LIMITS_PATCH_FILE}"
  } | awk '{print $3}' | LC_ALL=C sort -u)"
  actual_paths="$(git -C "${CONTRACTS_PATH}" diff --name-only | LC_ALL=C sort -u)"
  [[ -z "$(git -C "${CONTRACTS_PATH}" diff --cached --name-only)" \
    && -z "$(git -C "${CONTRACTS_PATH}" ls-files --others --exclude-standard)" \
    && "${actual_paths}" == "${expected_paths}" ]] \
    || { echo "error: era-contracts has changes outside the exact reviewed patch set" >&2; exit 1; }
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

# Both exact reverse checks must succeed after mutation (or on a repeated run).
base_patch_applied || { echo "error: base era-contracts patch post-state is partial or diverged" >&2; exit 1; }
da_limits_patch_applied || { echo "error: era-contracts DA-limits patch post-state is partial or diverged" >&2; exit 1; }
verify_canonical_verifier_artifacts
verify_only_expected_changes

if [[ "${changed}" == false ]]; then
  echo "era-contracts syscoin patch appears already applied; skipping."
  exit 0
fi

echo "Patch applied successfully."
