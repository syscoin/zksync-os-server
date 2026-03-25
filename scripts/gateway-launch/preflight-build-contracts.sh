#!/usr/bin/env bash
# Syscoin-era contracts patch + forge build + file_based genesis JSON.
# Requires: ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, REQUIRED_CONTRACTS_SHA,
#           REQUIRED_ZKSTACK_CLI_SHA, FOUNDRY_EVM_VERSION=shanghai
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_assert_contracts_sha
gl_assert_zksync_era_sha

bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-era-contracts-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}/contracts"

export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-shanghai}"
cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
forge build --skip test

mkdir -p "${ZKSYNC_ERA_PATH}/etc/env/file_based"
cd "${ZKSYNC_ERA_PATH}/contracts/tools/zksync-os-genesis-gen"
cargo run --release -- --output-file "${ZKSYNC_ERA_PATH}/etc/env/file_based/genesis.json"
