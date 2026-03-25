#!/usr/bin/env bash
# zkstack ecosystem create for Gateway (--zksync-os). Run from a dir with no ZkStack.yaml (e.g. $HOME).
# Requires: ZKSYNC_ERA_PATH, PATH with zkstack. Env overrides:
#   GATEWAY_ECOSYSTEM_NAME GATEWAY_CHAIN_NAME GATEWAY_CHAIN_ID GATEWAY_PROVER_MODE GATEWAY_COMMIT_MODE L1_NETWORK
#   GATEWAY_WALLET_CREATION GATEWAY_WALLET_PATH
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
gl_assert_zksync_era_sha
gl_assert_contracts_sha
gl_path_for_zkstack

: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_ECOSYSTEM_NAME:=$(basename "${GATEWAY_DIR}")}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${GATEWAY_CHAIN_ID:=57001}"
: "${GATEWAY_PROVER_MODE:=gpu}"
: "${GATEWAY_COMMIT_MODE:=rollup}"
: "${L1_NETWORK:=localhost}"
: "${GATEWAY_WALLET_CREATION:=}"
: "${GATEWAY_WALLET_PATH:=${GATEWAY_DIR}.wallets.yaml}"

if [ -z "${GATEWAY_WALLET_CREATION}" ]; then
  if [ -f "${GATEWAY_WALLET_PATH}" ]; then
    GATEWAY_WALLET_CREATION="in-file"
  else
    GATEWAY_WALLET_CREATION="random"
  fi
fi

if [ "${GATEWAY_WALLET_CREATION}" = "in-file" ]; then
  gl_require GATEWAY_WALLET_PATH
fi

cd "${GATEWAY_ECOSYSTEM_PARENT_DIR:-$(dirname "${GATEWAY_DIR}")}"

wallet_args=(--wallet-creation "${GATEWAY_WALLET_CREATION}")
if [ "${GATEWAY_WALLET_CREATION}" = "in-file" ]; then
  wallet_args+=(--wallet-path "${GATEWAY_WALLET_PATH}")
fi

zkstack ecosystem create \
  --ecosystem-name "${GATEWAY_ECOSYSTEM_NAME}" \
  --l1-network "${L1_NETWORK}" \
  --link-to-code "${ZKSYNC_ERA_PATH}" \
  --chain-name "${GATEWAY_CHAIN_NAME}" \
  --chain-id "${GATEWAY_CHAIN_ID}" \
  --prover-mode "${GATEWAY_PROVER_MODE}" \
  "${wallet_args[@]}" \
  --l1-batch-commit-data-generator-mode "${GATEWAY_COMMIT_MODE}" \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default true \
  --evm-emulator false \
  --start-containers false \
  --zksync-os

# `zkstack ecosystem create --link-to-code` runs a recursive submodule update on the linked
# checkout, which resets `contracts/` to the top-level repo's recorded submodule revision.
# Restore the versions.yaml-pinned contracts SHA before subsequent gateway-launch steps.
gl_checkout_contracts_sha
gl_assert_contracts_sha

if [ "${GATEWAY_WALLET_CREATION}" = "random" ] && [ ! -f "${GATEWAY_WALLET_PATH}" ]; then
  cp "${GATEWAY_DIR}/configs/wallets.yaml" "${GATEWAY_WALLET_PATH}"
  echo "gateway-launch: persisted ecosystem wallets to ${GATEWAY_WALLET_PATH}"
fi
