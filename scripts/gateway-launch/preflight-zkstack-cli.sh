#!/usr/bin/env bash
# Pin top-level zksync-era to versions.yaml's zkstack-cli.sha, verify/pin-compatible contracts checkout,
# apply the Syscoin patch, and build the repo-local zkstack CLI.
# Requires: ZKSYNC_OS_SERVER_PATH. Optional: ZKSYNC_ERA_PATH (else same default cache as run-gateway-launch.sh).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"

gl_ensure_zksync_era_workspace
gl_build_zkstack_cli_release
