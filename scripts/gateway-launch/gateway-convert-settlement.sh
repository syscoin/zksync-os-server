#!/usr/bin/env bash
# create-tx-filterer + convert-to-gateway (§4).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_path_for_zkstack
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
cd "${GATEWAY_DIR}"

zkstack chain gateway create-tx-filterer --chain "${GATEWAY_CHAIN_NAME}"
zkstack chain gateway convert-to-gateway --chain "${GATEWAY_CHAIN_NAME}"
