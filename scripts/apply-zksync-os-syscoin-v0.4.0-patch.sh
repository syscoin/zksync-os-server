#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/zksync-os" >&2
  exit 1
fi

ZKSYNC_OS_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-os-syscoin-v0.4.0.patch"
# SYSCOIN: The reviewed nested patch deliberately uses zero-context hunks so its
# own blank context lines cannot appear as trailing whitespace in this repository.
# Exact base/tree, byte, path-set, and postimage checks retain strict provenance.
EXPECTED_BASE_COMMIT="69bc430549e88f9264066d14f2001707572c5d33"
EXPECTED_BASE_TREE="233b36e77843e460ee9da3e344ee227fa8cce04a"
EXPECTED_PATCHED_TREE="25c44f3a9df994ef29d96638eca58eccf1df64da"
EXPECTED_PATCH_SIZE="1133789"
EXPECTED_PATCH_SHA256="38b06604a483d037542a88f1ab1caf1688d58a0520b3773a74ab6e4b3f64626d"
EXPECTED_PATCH_PATH_COUNT="53"
EXPECTED_PATCH_PATHS_SHA256="dc67052881ca18e7ef03b5142a704a627357e1cb55d21ec2725e06cd343b11ac"

die() {
  echo "error: $*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

compute_worktree_tree() {
  local temporary_dir temporary_index actual_tree
  temporary_dir="$(mktemp -d)" || die "failed to create a temporary index directory"
  temporary_index="${temporary_dir}/index"

  # SYSCOIN: Hash the complete worktree through an isolated index. The caller's real
  # index may intentionally still describe the pristine base and must remain untouched.
  if ! GIT_INDEX_FILE="${temporary_index}" \
    git -C "${ZKSYNC_OS_PATH}" read-tree HEAD; then
    rm -rf -- "${temporary_dir}"
    die "failed to initialize the isolated zksync-os worktree index"
  fi
  if ! GIT_INDEX_FILE="${temporary_index}" \
    git -C "${ZKSYNC_OS_PATH}" add --all; then
    rm -rf -- "${temporary_dir}"
    die "failed to populate the isolated zksync-os worktree index"
  fi
  if ! actual_tree="$(
    GIT_INDEX_FILE="${temporary_index}" \
      git -C "${ZKSYNC_OS_PATH}" write-tree
  )"; then
    rm -rf -- "${temporary_dir}"
    die "failed to compute the zksync-os worktree tree"
  fi

  rm -rf -- "${temporary_dir}"
  printf '%s\n' "${actual_tree}"
}

verify_exact_postimage_tree() {
  local actual_tree
  actual_tree="$(compute_worktree_tree)"
  [[ "${actual_tree}" == "${EXPECTED_PATCHED_TREE}" ]] || \
    die "wrong zksync-os patched worktree tree: expected=${EXPECTED_PATCHED_TREE} actual=${actual_tree}"
}

verify_postimage_provenance() {
  local actual_head actual_head_tree
  local -a commit_and_parents
  actual_head="$(git -C "${ZKSYNC_OS_PATH}" rev-parse HEAD)"
  actual_head_tree="$(git -C "${ZKSYNC_OS_PATH}" rev-parse 'HEAD^{tree}')"

  # SYSCOIN: Idempotent use is valid either on the reviewed base with the patch in
  # the worktree, or on a single local patch commit directly above that base.
  if [[ "${actual_head}" == "${EXPECTED_BASE_COMMIT}" ]]; then
    return
  fi
  read -r -a commit_and_parents <<< "$(
    git -C "${ZKSYNC_OS_PATH}" rev-list --parents -n 1 HEAD
  )"
  if [[ "${actual_head_tree}" == "${EXPECTED_PATCHED_TREE}" &&
    "${#commit_and_parents[@]}" -eq 2 &&
    "${commit_and_parents[1]}" == "${EXPECTED_BASE_COMMIT}" ]]; then
    return
  fi
  die "wrong zksync-os postimage provenance: head=${actual_head} tree=${actual_head_tree}"
}

require_text() {
  local relative_path="$1" expected="$2"
  local path="${ZKSYNC_OS_PATH}/${relative_path}"
  if [[ ! -f "${path}" || -L "${path}" ]]; then
    die "postimage is missing, not regular, or a symlink: ${relative_path}"
  fi
  grep -Fq -- "${expected}" "${path}" ||
    die "postimage is missing required attribution in ${relative_path}: ${expected}"
}

verify_semantics() {
  # SYSCOIN: Every production source touched downstream remains visibly attributable after a rebase.
  local tagged_path
  for tagged_path in \
    "basic_bootloader/src/bootloader/block_flow/zk/batch_data.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/block_data.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/blob_data_id_advice.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/blob_commitment_generator/mod.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/da_commitment_generator/mod.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/post_tx_op_proving_multiblock_batch.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/post_tx_op_proving_singleblock_batch.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/post_tx_op/public_input.rs" \
    "basic_bootloader/src/bootloader/block_flow/zk/tx_loop.rs" \
    "basic_bootloader/src/bootloader/constants.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/gas_helpers.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/zk/mod.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/zk/process_l1_transaction.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_gas_tank.rs" \
    "basic_bootloader/src/bootloader/transaction_flow/zk/validation_impl.rs" \
    "basic_system/Cargo.toml" \
    "basic_system/src/cost_constants.rs" \
    "basic_system/src/system_functions/mod.rs" \
    "basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs" \
    "callable_oracles/Cargo.toml" \
    "callable_oracles/src/blob_data_id/mod.rs" \
    "callable_oracles/src/lib.rs" \
    "evm_interpreter/Cargo.toml" \
    "evm_interpreter/src/precompile_addresses.rs" \
    "forward_system/Cargo.toml" \
    "forward_system/src/run/mod.rs" \
    "proof_running_system/Cargo.toml" \
    "system_hooks/Cargo.toml" \
    "system_hooks/src/lib.rs" \
    "zk_ee/src/common_structs/logs_storage.rs" \
    "zk_ee/src/system/base_system_functions.rs"
  do
    require_text "${tagged_path}" "SYSCOIN:"
  done
}

if ! git -C "${ZKSYNC_OS_PATH}" rev-parse --show-toplevel >/dev/null 2>&1; then
  echo "error: ${ZKSYNC_OS_PATH} is not a git repository" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  die "patch file not found: ${PATCH_FILE}"
fi

actual_patch_size="$(wc -c < "${PATCH_FILE}" | tr -d '[:space:]')"
actual_patch_sha256="$(sha256_file "${PATCH_FILE}")"
patch_paths="$(git apply --numstat --recount --unidiff-zero "${PATCH_FILE}" | cut -f3 | LC_ALL=C sort)"
patch_path_count="$(printf '%s\n' "${patch_paths}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
patch_paths_sha256="$(printf '%s\n' "${patch_paths}" | sha256_stdin)"
[[ "${actual_patch_size}" == "${EXPECTED_PATCH_SIZE}" ]] || \
  die "canonical patch size mismatch: expected=${EXPECTED_PATCH_SIZE} actual=${actual_patch_size}"
[[ "${actual_patch_sha256}" == "${EXPECTED_PATCH_SHA256}" ]] || \
  die "canonical patch SHA-256 mismatch: expected=${EXPECTED_PATCH_SHA256} actual=${actual_patch_sha256}"
[[ "${patch_path_count}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] || \
  die "canonical patch path count mismatch: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${patch_path_count}"
[[ "${patch_paths_sha256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] || \
  die "canonical patch path set mismatch: expected=${EXPECTED_PATCH_PATHS_SHA256} actual=${patch_paths_sha256}"

# SYSCOIN: Reverse applicability selects the idempotent state, but it does not
# constrain edits outside patch hunks. Provenance and the complete worktree tree do.
if git -C "${ZKSYNC_OS_PATH}" apply --reverse --check --recount --unidiff-zero "${PATCH_FILE}" >/dev/null 2>&1; then
  verify_postimage_provenance
  verify_exact_postimage_tree
  verify_semantics
  echo "zksync-os v0.4.0 Syscoin patch is already exact; skipping." >&2
  exit 0
fi

if [[ -n "$(git -C "${ZKSYNC_OS_PATH}" status --porcelain)" ]]; then
  die "zksync-os checkout must be clean before applying the Syscoin patch: ${ZKSYNC_OS_PATH}"
fi

actual_head="$(git -C "${ZKSYNC_OS_PATH}" rev-parse HEAD)"
actual_tree="$(git -C "${ZKSYNC_OS_PATH}" rev-parse 'HEAD^{tree}')"
[[ "${actual_head}" == "${EXPECTED_BASE_COMMIT}" ]] || \
  die "wrong zksync-os base commit: expected=${EXPECTED_BASE_COMMIT} actual=${actual_head}"
[[ "${actual_tree}" == "${EXPECTED_BASE_TREE}" ]] || \
  die "wrong zksync-os base tree: expected=${EXPECTED_BASE_TREE} actual=${actual_tree}"

echo "Checking zksync-os v0.4.0 patch applicability..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --check --recount --unidiff-zero "${PATCH_FILE}"

echo "Applying zksync-os v0.4.0 Syscoin patch..." >&2
git -C "${ZKSYNC_OS_PATH}" apply --recount --unidiff-zero "${PATCH_FILE}"
git -C "${ZKSYNC_OS_PATH}" apply --reverse --check --recount --unidiff-zero "${PATCH_FILE}" >/dev/null || {
  echo "error: zksync-os v0.4.0 Syscoin patch postimage failed reverse applicability" >&2
  exit 1
}
verify_postimage_provenance
verify_exact_postimage_tree
verify_semantics

echo "Patch applied successfully." >&2
