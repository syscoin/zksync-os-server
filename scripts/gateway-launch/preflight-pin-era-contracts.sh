#!/usr/bin/env bash
# One-time: pin zksync-era contracts submodule to REQUIRED_CONTRACTS_SHA (creates a git commit).
# Review before pushing. Requires: ZKSYNC_ERA_PATH, REQUIRED_CONTRACTS_SHA
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"

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
