#!/usr/bin/env bash
# Pin top-level zksync-era to versions.yaml's zkstack-cli.sha, verify/pin-compatible contracts checkout,
# apply the Syscoin patch, and build the repo-local zkstack CLI.
# Requires: ZKSYNC_ERA_PATH, ZKSYNC_OS_SERVER_PATH, REQUIRED_CONTRACTS_SHA
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"

current_head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD)"
if ! git -C "${ZKSYNC_ERA_PATH}" merge-base --is-ancestor "${REQUIRED_ZKSTACK_CLI_SHA}" "${current_head}" 2>/dev/null; then
  if [ -n "$(git -C "${ZKSYNC_ERA_PATH}" status --porcelain)" ]; then
    gl_die "zksync-era has local changes; cannot check out REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA}"
  fi
  git -C "${ZKSYNC_ERA_PATH}" fetch origin "${REQUIRED_ZKSTACK_CLI_SHA}"
  git -C "${ZKSYNC_ERA_PATH}" checkout "${REQUIRED_ZKSTACK_CLI_SHA}"
fi

gl_assert_zksync_era_sha
gl_assert_contracts_sha

bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-era-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}"
source "${HOME}/.cargo/env" >/dev/null 2>&1 || true

detect_nightly_toolchain() {
  if command -v rustup >/dev/null 2>&1; then
    rustup toolchain list | awk '/^nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}/ {print $1}' | sort -V | tail -n 1
  fi
}

: "${GATEWAY_ZKSTACK_CARGO_TOOLCHAIN:=$(detect_nightly_toolchain)}"
[ -n "${GATEWAY_ZKSTACK_CARGO_TOOLCHAIN}" ] || gl_die "no nightly Rust toolchain found; install one with rustup"

cd "${ZKSYNC_ERA_PATH}/zkstack_cli"
cargo +"${GATEWAY_ZKSTACK_CARGO_TOOLCHAIN}" build --release --locked -Znext-lockfile-bump -p zkstack
