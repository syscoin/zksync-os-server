#!/usr/bin/env bash
# zkstack chain init for the gateway chain (§3).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
# SYSCOIN: Initialize only the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
cd "${GATEWAY_DIR}"

# SYSCOIN: A direct invocation must authenticate both the selected L1 and the
# locally persisted Gateway identity before zkstack can broadcast chain init.
gl_validate_l1_network_pair
gl_normalize_canonical_deployment_inputs
gl_bind_gateway_launch_context
gl_assert_gateway_chain_config_matches_expected
gl_l1_broadcast_preflight

gl_zkstack_private_pty zkstack chain init \
  --chain "${GATEWAY_CHAIN_NAME}" \
  --no-genesis \
  --deploy-paymaster false \
  --l1-rpc-url "${L1_RPC_URL}"

if [ -f "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml" ]; then
  gl_secure_generated_wallet_file "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml"
fi

gl_ensure_chain_contracts_yaml_schema "${GATEWAY_CHAIN_NAME}"
gl_assert_gateway_chain_admin_ready
