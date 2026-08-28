#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/zksync-era" >&2
  exit 1
fi

ERA_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-era-syscoin.patch"

if ! ERA_ROOT="$(git -C "${ERA_PATH}" rev-parse --show-toplevel 2>/dev/null)" ||
  [[ "$(cd "${ERA_PATH}" && pwd -P)" != "$(cd "${ERA_ROOT}" && pwd -P)" ]]; then
  echo "error: ${ERA_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi

# SYSCOIN: The zkstack CLI is deployment-critical. Bind this patch to one exact
# upstream tree and one exact complete postimage; marker-only / per-file
# idempotency could accept a partial or locally modified deployment tool.
EXPECTED_BASE_COMMIT="d1f681c395a5b40fd4cfa591dea8ac3d3f80ebdc"
EXPECTED_BASE_TREE="6d8ac3b2867f9aeb561ba9a2174cd459d6362585"
EXPECTED_PATCH_SHA256="3524793b2e17754a7ed7113c9182006f19828816b794bc6374d9fc2b9eeb6182"
EXPECTED_PATCH_PATH_COUNT="18"
EXPECTED_PATCH_PATHS_SHA256="9780732ab76b6e1fc1065880896d9d06befb6c8a70ee13a23401becad9de0b4e"
EXPECTED_PATCHED_TREE="3b6a1e15e232f1e2986cb19d3fadcc6ab63d9a60"

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

actual_head="$(git -C "${ERA_PATH}" rev-parse HEAD)"
[[ "${actual_head}" == "${EXPECTED_BASE_COMMIT}" ]] || {
  echo "error: zksync-era HEAD mismatch: expected=${EXPECTED_BASE_COMMIT} actual=${actual_head}" >&2
  exit 1
}
actual_base_tree="$(git -C "${ERA_PATH}" rev-parse 'HEAD^{tree}')"
[[ "${actual_base_tree}" == "${EXPECTED_BASE_TREE}" ]] || {
  echo "error: zksync-era base tree mismatch: expected=${EXPECTED_BASE_TREE} actual=${actual_base_tree}" >&2
  exit 1
}
actual_patch_sha256="$(sha256_file "${PATCH_FILE}")"
[[ "${actual_patch_sha256}" == "${EXPECTED_PATCH_SHA256}" ]] || {
  echo "error: zksync-era patch digest mismatch: expected=${EXPECTED_PATCH_SHA256} actual=${actual_patch_sha256}" >&2
  exit 1
}

PATCH_PATHS="$(
  sed -nE 's|^diff --git a/([^ ]+) b/.*|\1|p' "${PATCH_FILE}" |
    LC_ALL=C sort
)"
actual_path_count="$(printf '%s\n' "${PATCH_PATHS}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
actual_paths_sha256="$(printf '%s\n' "${PATCH_PATHS}" | sha256_stdin)"
[[ "${actual_path_count}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] || {
  echo "error: zksync-era patch path count mismatch: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${actual_path_count}" >&2
  exit 1
}
[[ "${actual_paths_sha256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] || {
  echo "error: zksync-era patch path set mismatch" >&2
  exit 1
}

path_is_patch_input() {
  local candidate="$1" expected
  while IFS= read -r expected; do
    [[ "${candidate}" == "${expected}" ]] && return 0
  done <<<"${PATCH_PATHS}"
  return 1
}

verify_worktree_scope() {
  local line path
  git -C "${ERA_PATH}" diff --cached --quiet || {
    echo "error: staged zksync-era changes are not allowed" >&2
    exit 1
  }
  while IFS= read -r line; do
    [[ -n "${line}" ]] || continue
    path="${line:3}"
    case "${path}" in
    contracts | etc/env/file_based/genesis.json) continue ;;
    esac
    path_is_patch_input "${path}" || {
      echo "error: unrelated zksync-era worktree change: ${path}" >&2
      exit 1
    }
  done < <(git -C "${ERA_PATH}" status --porcelain --untracked-files=all)
}

patch_forward_applicable() {
  git -C "${ERA_PATH}" apply --check --whitespace=error-all "${PATCH_FILE}" >/dev/null 2>&1
}

patch_reverse_applicable() {
  git -C "${ERA_PATH}" apply --reverse --check "${PATCH_FILE}" >/dev/null 2>&1
}

verify_patched_tree() {
  local temporary_dir temporary_index actual_tree relative_path
  temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/syscoin-zkstack-patch-index.XXXXXX")"
  temporary_index="${temporary_dir}/index"
  if ! actual_tree="$({
    export GIT_INDEX_FILE="${temporary_index}"
    git -C "${ERA_PATH}" read-tree HEAD || exit 1
    while IFS= read -r relative_path; do
      [[ -n "${relative_path}" ]] || continue
      git -C "${ERA_PATH}" add -A -- "${relative_path}" || exit 1
    done <<<"${PATCH_PATHS}"
    git -C "${ERA_PATH}" write-tree || exit 1
  })"; then
    rm -f "${temporary_index}" "${temporary_index}.lock"
    rmdir "${temporary_dir}"
    echo "error: failed to calculate patched zksync-era tree" >&2
    exit 1
  fi
  rm -f "${temporary_index}" "${temporary_index}.lock"
  rmdir "${temporary_dir}"
  [[ "${actual_tree}" == "${EXPECTED_PATCHED_TREE}" ]] || {
    echo "error: patched zksync-era tree mismatch: expected=${EXPECTED_PATCHED_TREE} actual=${actual_tree}" >&2
    exit 1
  }
}

verify_worktree_scope
forward=false
reverse=false
patch_forward_applicable && forward=true
patch_reverse_applicable && reverse=true
[[ "${forward}" != "${reverse}" ]] || {
  echo "error: zksync-era patch state is partial, diverged, or ambiguous" >&2
  exit 1
}

if [[ "${forward}" == true ]]; then
  echo "Applying exact Syscoin/Tanenbaum zkstack compatibility patch..."
  git -C "${ERA_PATH}" apply --whitespace=error-all "${PATCH_FILE}"
else
  echo "Exact Syscoin/Tanenbaum zkstack patch is already applied."
fi

patch_reverse_applicable || {
  echo "error: zksync-era patch postimage failed reverse applicability" >&2
  exit 1
}
verify_worktree_scope
git -C "${ERA_PATH}" diff --check
verify_patched_tree
echo "Exact Syscoin/Tanenbaum zkstack source patch is attested: ${EXPECTED_PATCHED_TREE}."
