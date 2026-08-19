#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [ "$#" -lt 3 ] || [ "${2:-}" != "--" ]; then
  echo "Usage: $0 <workspace-name> -- <cargo args...>" >&2
  exit 1
fi

WORKSPACE_NAME="$1"
shift 2

case "${WORKSPACE_NAME}" in
  "" | *[!A-Za-z0-9._-]*)
    echo "error: workspace name must contain only letters, digits, '.', '_' or '-'" >&2
    exit 1
    ;;
esac

# These constants are the exact generated source inputs used for the published
# V7 application binaries. Deployment launchers derive and validate their own
# chain-specific values instead of using this static build wrapper.
if [ -z "${SYSCOIN_EDGE_DA_COMMIT_TARGET+x}" ]; then
  SYSCOIN_EDGE_DA_COMMIT_TARGET=0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19
fi
if [ -z "${SYSCOIN_GAS_TANK_ADDRESS+x}" ]; then
  SYSCOIN_GAS_TANK_ADDRESS=0xb9feff70ec42b6b5af5a690b4dbc332a2d1f3beb
fi
: "${PROTOCOL_VERSION:=v31.0}"
: "${ZKSYNC_OS_GIT_URL:=https://github.com/matter-labs/zksync-os.git}"
: "${GATEWAY_DIR:=${SYSCOIN_PATCHED_OS_BUILD_ROOT:-${TMPDIR:-/tmp}/syscoin-zksync-os-server-build}}"
: "${CARGO_TARGET_DIR:=${REPO_ROOT}/target}"

export CARGO_TARGET_DIR GATEWAY_DIR PROTOCOL_VERSION
export SYSCOIN_EDGE_DA_COMMIT_TARGET SYSCOIN_GAS_TANK_ADDRESS
export ZKSYNC_OS_GIT_URL
export ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true
export ZKSYNC_OS_SERVER_PATH="${REPO_ROOT}"
export ZKSYNC_OS_STATIC_BUILD_CONTEXT=true

exec bash "${SCRIPT_DIR}/gateway-launch/run-os-server-with-patched-zksync-os.sh" \
  "${WORKSPACE_NAME}" -- "$@"
