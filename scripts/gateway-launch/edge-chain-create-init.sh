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
gl_export_foundry_evm_version
: "${GATEWAY_DIR:=${HOME}/gateway}"
cd "${GATEWAY_DIR}"

: "${EDGE_CHAIN_NAME:=zksys}"
: "${EDGE_CHAIN_ID:=57057}"
: "${EDGE_PROVER_MODE:=}"
: "${EDGE_WALLET_CREATION:=}"
: "${EDGE_WALLET_PATH:=${GATEWAY_DIR}/.${EDGE_CHAIN_NAME}-wallets.yaml}"
if [ -z "${SKIP_FUND:-}" ]; then
  SKIP_FUND=false
fi

if [ -z "${EDGE_WALLET_CREATION}" ]; then
  if [ -f "${EDGE_WALLET_PATH}" ]; then
    EDGE_WALLET_CREATION="in-file"
  else
    EDGE_WALLET_CREATION="random"
  fi
fi

if [ -z "${EDGE_PROVER_MODE}" ]; then
  if [ "${PROVER_MODE}" = "no-proofs" ]; then
    EDGE_PROVER_MODE="no-proofs"
  else
    EDGE_PROVER_MODE="gpu"
  fi
fi

if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  gl_require EDGE_WALLET_PATH
fi

wallet_args=(--wallet-creation "${EDGE_WALLET_CREATION}")
if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  wallet_args+=(--wallet-path "${EDGE_WALLET_PATH}")
fi

if [ -f "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/ZkStack.yaml" ]; then
  echo "gateway-launch: edge chain ${EDGE_CHAIN_NAME} already exists; skipping chain create"
else
  zkstack chain create \
    --chain-name "${EDGE_CHAIN_NAME}" \
    --chain-id "${EDGE_CHAIN_ID}" \
    --prover-mode "${EDGE_PROVER_MODE}" \
    "${wallet_args[@]}" \
    --l1-batch-commit-data-generator-mode rollup \
    --base-token-address 0x0000000000000000000000000000000000000001 \
    --base-token-price-nominator 1 \
    --base-token-price-denominator 1 \
    --set-as-default false \
    --evm-emulator false \
    --zksync-os

  if [ "${EDGE_WALLET_CREATION}" = "random" ] && [ ! -f "${EDGE_WALLET_PATH}" ]; then
    cp "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" "${EDGE_WALLET_PATH}"
    echo "gateway-launch: persisted edge wallets to ${EDGE_WALLET_PATH}"
  fi
fi

if [ "${SKIP_FUND}" != "true" ]; then
  GATEWAY_CHAIN_NAME="${EDGE_CHAIN_NAME}" "${SCRIPT_DIR}/fund-wallets.sh"
else
  echo "gateway-launch: SKIP_FUND=true, skipping edge wallet funding"
fi

init_output=""
if ! init_output="$(zkstack chain init \
  --chain "${EDGE_CHAIN_NAME}" \
  --no-genesis \
  --deploy-paymaster false \
  --skip-priority-txs \
  --l1-rpc-url "${L1_RPC_URL}" 2>&1)"; then
  init_output_lc="$(gl_to_lower "${init_output}")"
  echo "${init_output}"
  case "${init_output_lc}" in
  *"already initialized"* | *"already deployed"* | *"already exists"*)
    echo "gateway-launch: edge chain ${EDGE_CHAIN_NAME} is already initialized; continuing"
    ;;
  *)
    exit 1
    ;;
  esac
else
  echo "${init_output}"
fi

gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"
