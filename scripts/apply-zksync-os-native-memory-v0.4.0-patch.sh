#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || { echo "Usage: $0 /absolute/path/to/disposable-native-zksync-os" >&2; exit 1; }
NATIVE_OS_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-os-native-memory-v0.4.0.patch"

# SYSCOIN: This overlay is NOT a new guest identity. First attest the published
# canonical guest source, then apply exactly two native-only capture files in a
# separate disposable checkout. The canonical checkout and keygen evidence stay intact.
EXPECTED_LOCKED_BASE="69bc430549e88f9264066d14f2001707572c5d33"
EXPECTED_CANONICAL_TREE="6935489bdbc7b1ed31e608677d1b2418b10691b5"
EXPECTED_NATIVE_TREE="6e86060d1182dc198681c40b2c6dcfc39a9b58fd"
EXPECTED_PATCH_SIZE="23671"
EXPECTED_PATCH_SHA256="9e86cac64578da70181685602dcdc4823226ee398cdc75550e4ff9d9a0cd7728"
EXPECTED_PATCH_PATHS_SHA256="371346e0908b214c7daac4d9a638925a9cde35228160a88b465f1d8db186e33d"
EXPECTED_FORWARD_SHA256="fbae5afd8101884cde9eb4e2b75aedbad5c771aad60ec7ba03baa7c0c5daac78"
EXPECTED_ORACLE_SHA256="f91692ee8f99a80fd64917e246d0744cf33059b9640a893fbfe3ec146efbbc9a"

die() { echo "error: $*" >&2; exit 1; }
sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}';
  else shasum -a 256 | awk '{print $1}'; fi
}
sha256_file() { sha256_stdin < "$1"; }

[[ -f "${PATCH_FILE}" && ! -L "${PATCH_FILE}" ]] || die "native-memory patch must be a regular non-symlink file"
[[ "$(wc -c < "${PATCH_FILE}" | tr -d '[:space:]')" = "${EXPECTED_PATCH_SIZE}" ]] || die "native-memory patch size mismatch"
[[ "$(sha256_file "${PATCH_FILE}")" = "${EXPECTED_PATCH_SHA256}" ]] || die "native-memory patch SHA-256 mismatch"
patch_paths="$(git apply --numstat --recount --unidiff-zero "${PATCH_FILE}" | cut -f3 | LC_ALL=C sort)"
expected_paths=$'forward_system/src/run/mod.rs\noracle_provider/src/lib.rs'
[[ "${patch_paths}" = "${expected_paths}" ]] || die "native-memory patch changes paths outside the closed two-file allowlist"
[[ "$(printf '%s\n' "${patch_paths}" | sha256_stdin)" = "${EXPECTED_PATCH_PATHS_SHA256}" ]] || die "native-memory patch path digest mismatch"

repo_root="$(git -C "${NATIVE_OS_PATH}" rev-parse --show-toplevel 2>/dev/null)" || die "native source is not a Git repository"
[[ "$(cd "${NATIVE_OS_PATH}" && pwd -P)" = "$(cd "${repo_root}" && pwd -P)" ]] || die "native source must be its repository root"
head_tree="$(git -C "${NATIVE_OS_PATH}" rev-parse 'HEAD^{tree}')"
case "${head_tree}" in
  "${EXPECTED_CANONICAL_TREE}")
    canonical_rev="$(git -C "${NATIVE_OS_PATH}" rev-parse HEAD)"
    ;;
  "${EXPECTED_NATIVE_TREE}")
    canonical_rev="$(git -C "${NATIVE_OS_PATH}" rev-parse HEAD^)"
    [[ "$(git -C "${NATIVE_OS_PATH}" rev-parse 'HEAD^{}~1^{tree}')" = "${EXPECTED_CANONICAL_TREE}" ]] || die "native overlay commit is not directly based on canonical source"
    ;;
  *) die "native overlay requires the exact canonical or attested native tree, got ${head_tree}" ;;
esac
[[ "$(git -C "${NATIVE_OS_PATH}" rev-parse "${canonical_rev}^")" = "${EXPECTED_LOCKED_BASE}" ]] || die "canonical native-overlay parent is not the locked official base"

compute_worktree_tree() {
  local index_dir actual_tree
  index_dir="$(mktemp -d "${TMPDIR:-/tmp}/syscoin-native-memory-index.XXXXXX")"
  # No change to the caller's index; all non-ignored tracked/untracked files are attested.
  if ! GIT_INDEX_FILE="${index_dir}/index" git -C "${NATIVE_OS_PATH}" read-tree HEAD ||
     ! GIT_INDEX_FILE="${index_dir}/index" git -C "${NATIVE_OS_PATH}" add --all ||
     ! actual_tree="$(GIT_INDEX_FILE="${index_dir}/index" git -C "${NATIVE_OS_PATH}" write-tree)"; then
    rm -f "${index_dir}/index" "${index_dir}/index.lock"
    rmdir "${index_dir}"
    die "failed to attest native-memory worktree"
  fi
  rm -f "${index_dir}/index" "${index_dir}/index.lock"
  rmdir "${index_dir}"
  printf '%s\n' "${actual_tree}"
}

worktree_tree="$(compute_worktree_tree)"
if [[ "${worktree_tree}" = "${EXPECTED_CANONICAL_TREE}" ]]; then
  for native_source in forward_system/src/run/mod.rs oracle_provider/src/lib.rs; do
    [[ -f "${NATIVE_OS_PATH}/${native_source}" && ! -L "${NATIVE_OS_PATH}/${native_source}" ]] || die "native overlay preimage is not a regular file: ${native_source}"
  done
  git -C "${NATIVE_OS_PATH}" apply --check --recount --unidiff-zero --whitespace=error "${PATCH_FILE}" || die "native-memory patch preimage mismatch"
  git -C "${NATIVE_OS_PATH}" apply --recount --unidiff-zero --whitespace=error "${PATCH_FILE}"
elif [[ "${worktree_tree}" != "${EXPECTED_NATIVE_TREE}" ]]; then
  die "native-memory worktree contains partial or unrelated changes: ${worktree_tree}"
fi

[[ "$(compute_worktree_tree)" = "${EXPECTED_NATIVE_TREE}" ]] || die "native-memory overlay postimage tree mismatch"
changed_paths="$(git -C "${NATIVE_OS_PATH}" diff --name-only "${canonical_rev}" "${EXPECTED_NATIVE_TREE}" | LC_ALL=C sort)"
[[ "${changed_paths}" = "${expected_paths}" ]] || die "native overlay changes a guest or non-allowlisted path"
[[ "$(sha256_file "${NATIVE_OS_PATH}/forward_system/src/run/mod.rs")" = "${EXPECTED_FORWARD_SHA256}" ]] || die "native bounded runner postimage SHA-256 mismatch"
[[ "$(sha256_file "${NATIVE_OS_PATH}/oracle_provider/src/lib.rs")" = "${EXPECTED_ORACLE_SHA256}" ]] || die "native bounded oracle postimage SHA-256 mismatch"
echo "Native-only witness-memory overlay is exact; published guest source is unchanged." >&2
