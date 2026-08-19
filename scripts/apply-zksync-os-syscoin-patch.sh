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
    && has_text "pub const ENCODABLE_BYTES_PER_BLOB: usize = 2 * 1024 * 1024;" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/mod.rs" \
    && has_text "blobs_advice.push(8);" "${ZKSYNC_OS_PATH}/forward_system/src/run/mod.rs"
}

canonical_upgrade_fix_applied() {
  has_text "canonical_upgrade_tx_hash: Bytes32::ZERO," "${ZKSYNC_OS_PATH}/zk_ee/src/system/metadata/zk_metadata.rs" \
    && has_text "recorded_upgrade_tx_hash" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/post_tx_op_proving_singleblock_batch.rs" \
    && has_text "canonical upgrade tx hash mismatch" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/post_tx_op_proving_singleblock_batch.rs" \
    && has_text "pub fn syscoin_compact_edge_da_commit_target() -> B160" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs" \
    && has_text "pub fn syscoin_gas_tank_address() -> B160" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs" \
    && has_text "syscoin_edge_da::syscoin_compact_edge_da_commit_target" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/mod.rs" \
    && has_text "apply_syscoin_edge_da_refs_root_for_tx_numbers" "${ZKSYNC_OS_PATH}/zk_ee/src/common_structs/logs_storage.rs"
}

gas_tank_patch_applied() {
  has_text "pub fn try_precharge_from_gas_tank" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_gas_tank.rs" \
    && has_text "fee_paid_from_gas_tank" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/mod.rs" \
    && has_text "SYSCOIN_GAS_TANK_INTRINSIC_PUBDATA" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/constants.rs" \
    && has_text "native SYS payments keep the upstream full-gas-price operator payment behavior" "${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/mod.rs"
}

slh_dsa_precompile_applied() {
  has_text "slh_dsa_precompile = [\"evm_interpreter/slh_dsa_precompile\"]" "${ZKSYNC_OS_PATH}/system_hooks/Cargo.toml" \
    && has_text "SLH_DSA_SHA2_128_24_VERIFY_HOOK_ADDRESS_LOW" "${ZKSYNC_OS_PATH}/evm_interpreter/src/precompile_addresses.rs" \
    && has_text "SlhDsaSha212824VerifyImpl" "${ZKSYNC_OS_PATH}/basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs" \
    && has_text "compress256(state, core::slice::from_ref(&block));" "${ZKSYNC_OS_PATH}/basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs" \
    && has_text "system_hooks/slh_dsa_precompile" "${ZKSYNC_OS_PATH}/forward_system/Cargo.toml"
}

if git -C "${ZKSYNC_OS_PATH}" apply --reverse --check --recount "${PATCH_FILE}" >/dev/null 2>&1; then
  echo "zksync-os Syscoin patch is already exact; skipping." >&2
  exit 0
fi

if base_patch_applied && { ! canonical_upgrade_fix_applied || ! slh_dsa_precompile_applied || ! gas_tank_patch_applied; }; then
  echo "error: detected an older partially applied Syscoin patch in ${ZKSYNC_OS_PATH}." >&2
  echo "Please start from a clean upstream checkout/tag before applying the updated patch." >&2
  exit 1
fi

if [[ -n "$(git -C "${ZKSYNC_OS_PATH}" status --porcelain)" ]]; then
  echo "error: zksync-os checkout must be clean before applying the Syscoin patch: ${ZKSYNC_OS_PATH}" >&2
  exit 1
fi

echo "Checking patch applicability..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --check --recount "${PATCH_FILE}"

echo "Applying zksync-os Syscoin patch..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --recount "${PATCH_FILE}"
git -C "${ZKSYNC_OS_PATH}" apply --reverse --check --recount "${PATCH_FILE}" >/dev/null || {
  echo "error: zksync-os Syscoin patch postimage failed reverse applicability" >&2
  exit 1
}

echo "Patch applied successfully." >&2
