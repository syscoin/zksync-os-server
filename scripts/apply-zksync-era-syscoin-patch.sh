#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/zksync-era" >&2
  exit 1
fi

ERA_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-era-syscoin.patch"

if [[ ! -d "${ERA_PATH}/.git" ]]; then
  echo "error: ${ERA_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi

if rg -n "Tanenbaum" "${ERA_PATH}/core/lib/basic_types/src/network.rs" >/dev/null 2>&1 \
  && rg -n "Tanenbaum" "${ERA_PATH}/zkstack_cli/crates/types/src/l1_network.rs" >/dev/null 2>&1; then
  echo "zksync-era Syscoin patch appears already applied; skipping."
  exit 0
fi

echo "Checking patch applicability..."
git -C "${ERA_PATH}" apply --check "${PATCH_FILE}"

echo "Applying Syscoin/Tanenbaum compatibility patch..."
git -C "${ERA_PATH}" apply "${PATCH_FILE}"

echo "Patch applied successfully."
