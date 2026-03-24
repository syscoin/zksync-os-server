#!/usr/bin/env bash
# After a phantom/partial L1 deploy on disposable Anvil: wipe forge artifacts, re-fund, re-run L1 deploy only.
# You must restart Anvil with a fresh chain first. Keeps existing GATEWAY_DIR.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require ZKSYNC_ERA_PATH
gl_require GATEWAY_DIR
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"

rm -rf "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/broadcast" "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
mkdir -p "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"

"${SCRIPT_DIR}/fund-wallets.sh"
"${SCRIPT_DIR}/gateway-deploy-l1.sh"
