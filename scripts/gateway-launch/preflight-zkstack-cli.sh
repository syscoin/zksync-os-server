#!/usr/bin/env bash
# Syscoin patch on zksync-era checkout + install zkstack CLI (zkstackup). Run once per machine/clone.
# Requires: ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH
set -euo pipefail
gl_require() { [ -n "${!1:-}" ] || {
  echo "unset: $1" >&2
  exit 1
}; }
gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH

bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-era-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}"
curl -fsSL https://raw.githubusercontent.com/matter-labs/zksync-era/main/zkstack_cli/zkstackup/install | bash
cd "${ZKSYNC_ERA_PATH}" && zkstackup --local
