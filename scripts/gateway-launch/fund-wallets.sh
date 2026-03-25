#!/usr/bin/env bash
# Fund addresses in ${GATEWAY_DIR}/configs/wallets.yaml on L1_RPC_URL.
# Default funder: Anvil dev key 0. Override: FUNDER_PRIVATE_KEY
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require GATEWAY_DIR
gl_require L1_RPC_URL
gl_path_for_zkstack
: "${GATEWAY_CHAIN_NAME:=gateway}"
gl_fund_wallets_yaml
CHAIN_WALLETS_YAML="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml"
if [ -f "${CHAIN_WALLETS_YAML}" ]; then
  WALLETS_YAML_PATH="${CHAIN_WALLETS_YAML}" gl_fund_wallets_yaml
fi
