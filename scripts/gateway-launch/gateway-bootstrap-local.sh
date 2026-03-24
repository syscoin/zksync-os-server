#!/usr/bin/env bash
# Disposable local Anvil path: create ecosystem under $HOME/gateway, fund wallets, deploy L1 (§1+§2).
# Prerequisites: Anvil running (anvil-local-start.sh), watch optional (anvil-watch-pending.sh).
# Requires: ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, PROTOCOL_VERSION, L1_RPC_URL, L1_CHAIN_ID=9
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
gl_assert_contracts_sha

export L1_CHAIN_ID="${L1_CHAIN_ID:-9}"
export L1_RPC_URL="${L1_RPC_URL:-http://127.0.0.1:8545}"
export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-shanghai}"
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"

cd "${HOME}"
"${SCRIPT_DIR}/gateway-ecosystem-create.sh"

export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
test -f "${GATEWAY_DIR}/ZkStack.yaml"

"${SCRIPT_DIR}/fund-wallets.sh"
"${SCRIPT_DIR}/gateway-deploy-l1.sh"
