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

_any=false
for w in "${ROOT_W}" "${CHAIN_W}"; do
  if [ -f "$w" ]; then
    _any=true
    echo "gateway-launch: funding wallets from ${w}"
    WALLETS_YAML_PATH="$w" gl_fund_wallets_yaml
  fi
done
if [ "${_any}" = false ]; then
  gl_die "no wallets.yaml found (tried ${ROOT_W} and ${CHAIN_W})"
fi
