#!/usr/bin/env bash
# zkstack ecosystem create for Gateway (--zksync-os). Run from a dir with no ZkStack.yaml (e.g. $HOME).
# Requires: ZKSYNC_ERA_PATH, PATH with zkstack. Env overrides:
#   GATEWAY_ECOSYSTEM_NAME GATEWAY_CHAIN_NAME GATEWAY_CHAIN_ID GATEWAY_PROVER_MODE GATEWAY_COMMIT_MODE L1_NETWORK
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
gl_path_for_zkstack

: "${GATEWAY_ECOSYSTEM_NAME:=gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${GATEWAY_CHAIN_ID:=57001}"
: "${GATEWAY_PROVER_MODE:=gpu}"
: "${GATEWAY_COMMIT_MODE:=rollup}"
: "${L1_NETWORK:=localhost}"

cd "${GATEWAY_ECOSYSTEM_PARENT_DIR:-${HOME}}"

zkstack ecosystem create \
  --ecosystem-name "${GATEWAY_ECOSYSTEM_NAME}" \
  --l1-network "${L1_NETWORK}" \
  --link-to-code "${ZKSYNC_ERA_PATH}" \
  --chain-name "${GATEWAY_CHAIN_NAME}" \
  --chain-id "${GATEWAY_CHAIN_ID}" \
  --prover-mode "${GATEWAY_PROVER_MODE}" \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode "${GATEWAY_COMMIT_MODE}" \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default true \
  --evm-emulator false \
  --start-containers false \
  --zksync-os
