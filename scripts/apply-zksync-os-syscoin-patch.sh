#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/zksync-os" >&2
  exit 1
fi

ZKSYNC_OS_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-os-syscoin.patch"

if ! git -C "${ZKSYNC_OS_PATH}" rev-parse --show-toplevel >/dev/null 2>&1; then
  echo "error: ${ZKSYNC_OS_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi

has_text() {
  local needle="$1"
  local file="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q --fixed-strings "$needle" "$file"
  else
    grep -q --fixed-strings "$needle" "$file"
  fi
}

base_patch_applied() {
  has_text "Blob data id advice mismatch" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/mod.rs" \
    && has_text "const USIZE_LEN: usize = 32 / size_of::<usize>();" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/commitment_and_proof_advice.rs" \
    && has_text "SYSCOIN: Keep the legacy function name/interface, but return blob data id" "${ZKSYNC_OS_PATH}/callable_oracles/src/blob_kzg_commitment/mod.rs" \
    && has_text "blobs_advice.push(8);" "${ZKSYNC_OS_PATH}/forward_system/src/run/mod.rs"
}

canonical_upgrade_fix_applied() {
  has_text "canonical_upgrade_tx_hash: Bytes32::ZERO," "${ZKSYNC_OS_PATH}/zk_ee/src/system/metadata/zk_metadata.rs" \
    && has_text "recorded_upgrade_tx_hash" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/post_tx_op_proving_singleblock_batch.rs"
}

if base_patch_applied && canonical_upgrade_fix_applied; then
  echo "zksync-os Syscoin patch appears already applied; skipping." >&2
  exit 0
fi

if base_patch_applied && ! canonical_upgrade_fix_applied; then
  echo "error: detected an older partially applied Syscoin patch in ${ZKSYNC_OS_PATH}." >&2
  echo "Please start from a clean upstream checkout/tag before applying the updated patch." >&2
  exit 1
fi

echo "Checking patch applicability..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --check --recount "${PATCH_FILE}"

echo "Applying zksync-os Syscoin patch..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --recount "${PATCH_FILE}"

echo "Patch applied successfully." >&2
