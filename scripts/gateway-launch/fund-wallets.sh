#!/usr/bin/env bash
# Fund addresses in wallets.yaml on L1_RPC_URL.
# zkstack --zksync-os may only create chains/<name>/configs/wallets.yaml (no top-level configs/).
# Default funder: Anvil dev key 0. Override: FUNDER_PRIVATE_KEY
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require GATEWAY_DIR
gl_require L1_RPC_URL
gl_path_for_zkstack
: "${GATEWAY_CHAIN_NAME:=gateway}"

ROOT_W="${GATEWAY_DIR}/configs/wallets.yaml"
CHAIN_W="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml"

if [ "${GATEWAY_CHAIN_NAME}" = "gateway" ]; then
  PRIMARY_W="${ROOT_W}"
  FALLBACK_W="${CHAIN_W}"
else
  PRIMARY_W="${CHAIN_W}"
  FALLBACK_W="${ROOT_W}"
fi

if [ -f "${PRIMARY_W}" ]; then
  echo "gateway-launch: funding wallets from ${PRIMARY_W}"
  WALLETS_YAML_PATH="${PRIMARY_W}" gl_fund_wallets_yaml
elif [ -f "${FALLBACK_W}" ]; then
  echo "gateway-launch: funding wallets from ${FALLBACK_W}"
  WALLETS_YAML_PATH="${FALLBACK_W}" gl_fund_wallets_yaml
else
  gl_die "no wallets.yaml found (tried ${ROOT_W} and ${CHAIN_W})"
fi
