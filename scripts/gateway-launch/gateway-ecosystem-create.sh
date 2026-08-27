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
# SYSCOIN: Create only the canonical fresh V32 ecosystem.
: "${PROTOCOL_VERSION:=v32.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
gl_assert_zksync_era_sha
gl_assert_contracts_sha
gl_ensure_zkstack_cli_release_current
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
gl_normalize_canonical_deployment_inputs
gl_reject_no_proofs_on_mainnet
gl_resolve_gateway_dir planned
gl_acquire_gateway_launch_lock

if [ -z "${GATEWAY_WALLET_CREATION}" ]; then
  GATEWAY_WALLET_CREATION="$(gl_wallet_creation_for_path "${GATEWAY_WALLET_PATH}")"
fi

if [ "${GATEWAY_WALLET_CREATION}" = "in-file" ]; then
  gl_require GATEWAY_WALLET_PATH
  gl_prepare_wallet_file_for_in_file "${GATEWAY_WALLET_PATH}"
fi

cd "${GATEWAY_ECOSYSTEM_PARENT_DIR:-$(dirname "${GATEWAY_DIR}")}"

wallet_args=(--wallet-creation "${GATEWAY_WALLET_CREATION}")
if [ "${GATEWAY_WALLET_CREATION}" = "in-file" ]; then
  wallet_args+=(--wallet-path "${GATEWAY_WALLET_PATH}")
fi

# SYSCOIN: the pinned era superproject records an older contracts gitlink than
# the independently pinned, attested Syscoin contracts postimage. Scope Git's
# documented `update=none` policy to this child process so zkstack preserves
# only that submodule while continuing to initialize its other submodules.
gl_zkstack_private_pty env \
  GIT_CONFIG_COUNT=1 \
  GIT_CONFIG_KEY_0=submodule.contracts.update \
  GIT_CONFIG_VALUE_0=none \
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

# SYSCOIN: zkstack normalizes filesystem-unsafe ecosystem names (for example,
# `gateway-v32-test` to `gateway_v32_test`). Resolve that emitted directory
# before hardening or persisting its generated private-key files.
gl_resolve_gateway_dir
gl_secure_generated_wallet_file "${GATEWAY_DIR}/configs/wallets.yaml"
if [ -f "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml" ]; then
  gl_secure_generated_wallet_file "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml"
fi
gl_bind_gateway_launch_context
gl_assert_gateway_chain_config_matches_expected

if [ "${GATEWAY_WALLET_CREATION}" = "random" ] && [ ! -e "${GATEWAY_WALLET_PATH}" ] && [ ! -L "${GATEWAY_WALLET_PATH}" ]; then
  gl_persist_wallet_file "${GATEWAY_DIR}/configs/wallets.yaml" "${GATEWAY_WALLET_PATH}"
  echo "gateway-launch: persisted ecosystem wallets to ${GATEWAY_WALLET_PATH}"
fi

# Re-attest both the contracts HEAD and the complete reviewed postimage after
# returning from the subprocess; the scoped Git policy must not become trust.
gl_ensure_era_contracts_syscoin_postimage
gl_assert_contracts_sha
