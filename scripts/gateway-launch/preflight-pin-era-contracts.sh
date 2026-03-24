#!/usr/bin/env bash
# One-time: pin zksync-era contracts submodule to REQUIRED_CONTRACTS_SHA (creates a git commit).
# Review before pushing. Requires: ZKSYNC_ERA_PATH, REQUIRED_CONTRACTS_SHA
set -euo pipefail
gl_require() { [ -n "${!1:-}" ] || {
  echo "unset: $1" >&2
  exit 1
}; }
gl_require ZKSYNC_ERA_PATH
gl_require REQUIRED_CONTRACTS_SHA

cd "${ZKSYNC_ERA_PATH}"
git submodule update --init contracts
cd "${ZKSYNC_ERA_PATH}/contracts"
git fetch origin "${REQUIRED_CONTRACTS_SHA}"
git checkout "${REQUIRED_CONTRACTS_SHA}"
git submodule sync --recursive
git submodule update --init --recursive
EXPECTED_NESTED_SHA="$(git ls-tree HEAD lib/@matterlabs/zksync-contracts | awk '{print $3}')"
test "$(git -C lib/@matterlabs/zksync-contracts rev-parse HEAD)" = "${EXPECTED_NESTED_SHA}"
test "$(git rev-parse HEAD)" = "${REQUIRED_CONTRACTS_SHA}"
cd "${ZKSYNC_ERA_PATH}"
git add contracts && git commit -m "chore(local): pin contracts to ${REQUIRED_CONTRACTS_SHA}"
