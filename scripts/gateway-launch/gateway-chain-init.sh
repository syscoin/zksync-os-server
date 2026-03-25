#!/usr/bin/env bash
# zkstack chain init for the gateway chain (§3).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
gl_require L1_RPC_URL
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_assert_zksync_era_sha
gl_path_for_zkstack
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
cd "${GATEWAY_DIR}"

gl_zkstack_pty zkstack chain init \
  --chain "${GATEWAY_CHAIN_NAME}" \
  --no-genesis \
  --deploy-paymaster false \
  --l1-rpc-url "${L1_RPC_URL}"
