#!/usr/bin/env bash
# Create and zkstack-init an edge (child) chain under the ecosystem (§5).
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
cd "${GATEWAY_DIR}"

: "${EDGE_CHAIN_NAME:=zksys}"
: "${EDGE_CHAIN_ID:=57057}"

zkstack chain create \
  --chain-name "${EDGE_CHAIN_NAME}" \
  --chain-id "${EDGE_CHAIN_ID}" \
  --prover-mode gpu \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode rollup \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default false \
  --evm-emulator false \
  --zksync-os

GATEWAY_CHAIN_NAME="${EDGE_CHAIN_NAME}" "${SCRIPT_DIR}/fund-wallets.sh"

zkstack chain init \
  --chain "${EDGE_CHAIN_NAME}" \
  --no-genesis \
  --deploy-paymaster false \
  --skip-priority-txs \
  --l1-rpc-url "${L1_RPC_URL}"
