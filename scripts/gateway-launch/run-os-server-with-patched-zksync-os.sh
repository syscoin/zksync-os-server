#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/../_patched-zksync-os-workspace.sh"

usage() {
  cat <<'EOF' >&2
Usage:
  run-os-server-with-patched-zksync-os.sh <workspace-name> -- <cargo args...>
  run-os-server-with-patched-zksync-os.sh <workspace-name> -- build-prebuilt
  run-os-server-with-patched-zksync-os.sh <workspace-name> -- exec-prebuilt -- <binary args...>
Examples:
  run-os-server-with-patched-zksync-os.sh gateway -- build-prebuilt
  run-os-server-with-patched-zksync-os.sh gateway -- run --release -- --config /path/to/config.yaml
  run-os-server-with-patched-zksync-os.sh gateway -- exec-prebuilt -- --config /path/to/config.yaml
EOF
  exit 1
}

[ $# -ge 3 ] || usage
WORKSPACE_NAME="$1"
shift
[ "${1:-}" = "--" ] || usage
shift
[ $# -gt 0 ] || usage

gl_require GATEWAY_DIR
gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
: "${ZKSYNC_OS_GIT_URL:=https://github.com/matter-labs/zksync-os.git}"

protocol_uses_dev_patch() {
  case "${PROTOCOL_VERSION}" in
  v31.* | v32.*) return 0 ;;
  *) return 1 ;;
  esac
}

uses_patched_workspace() {
  [ "${ZKSYNC_OS_FORCE_PATCHED_WORKSPACE:-false}" = "true" ] || \
    protocol_uses_dev_patch
}

prebuilt_binary_path() {
  printf '%s\n' "${GATEWAY_DIR}/.gateway-launch/target/${WORKSPACE_NAME}/release/zksync-os-server"
}

configure_build_context() {
  uses_patched_workspace || return 0
  gl_export_syscoin_edge_da_commit_target_from_gateway_config
  if [ "${ZKSYNC_OS_STATIC_BUILD_CONTEXT:-false}" = "true" ]; then
    local gas_tank primary_gas_tank secondary_gas_tank
    primary_gas_tank="${SYSCOIN_GAS_TANK_ADDRESS:-}"
    secondary_gas_tank="${ZKSYNC_OS_SYSCOIN_GAS_TANK_ADDRESS:-}"
    if [ -n "${primary_gas_tank}" ] && [ -n "${secondary_gas_tank}" ]; then
      [ "$(gl_normalize_syscoin_gas_tank_address "${primary_gas_tank}")" = \
        "$(gl_normalize_syscoin_gas_tank_address "${secondary_gas_tank}")" ] || \
        gl_die "static Syscoin gas-tank build inputs disagree"
    fi
    gas_tank="${primary_gas_tank:-${secondary_gas_tank}}"
    if [ -n "${gas_tank}" ]; then
      export SYSCOIN_GAS_TANK_ADDRESS
      SYSCOIN_GAS_TANK_ADDRESS="$(gl_normalize_syscoin_gas_tank_address "${gas_tank}")"
      unset ZKSYNC_OS_SYSCOIN_GAS_TANK_ADDRESS
    else
      unset SYSCOIN_GAS_TANK_ADDRESS ZKSYNC_OS_SYSCOIN_GAS_TANK_ADDRESS
    fi
    return 0
  fi
  case "${WORKSPACE_NAME}" in
  "${EDGE_CHAIN_NAME:-zksys}" | "${EDGE_CHAIN_NAME:-zksys}"-*)
    gl_export_syscoin_gas_tank_address_from_edge_config
    ;;
  *)
    # SYSCOIN: the zkSYS gas tank is edge-chain specific. Gateway nodes using
    # the same patched OS must keep the generated gas-tank constant at zero.
    unset SYSCOIN_GAS_TANK_ADDRESS ZKSYNC_OS_SYSCOIN_GAS_TANK_ADDRESS
    ;;
  esac
}

prebuilt_digest() {
  local binary="$1" binary_sha256
  binary_sha256="$(sha256sum "${binary}" | awk '{print $1}')"
  printf '%s\0%s\0%s\0%s\0%s\0' \
    "${binary_sha256}" \
    "${WORKSPACE_NAME}" \
    "${PROTOCOL_VERSION}" \
    "${SYSCOIN_EDGE_DA_COMMIT_TARGET:-}" \
    "${SYSCOIN_GAS_TANK_ADDRESS:-}" |
    sha256sum | awk '{print $1}'
}

refresh_os_server_config_credentials() {
  local seen_bin_args=false expect_config=false arg config_entry config_path
  local config_paths=()

  for arg in "$@"; do
    if [ "${arg}" = "--" ]; then
      seen_bin_args=true
      expect_config=false
      continue
    fi
    [ "${seen_bin_args}" = true ] || continue

    if [ "${expect_config}" = true ]; then
      config_paths+=("${arg}")
      expect_config=false
      continue
    fi

    case "${arg}" in
    --config=*)
      config_paths+=("${arg#--config=}")
      ;;
    --config)
      expect_config=true
      ;;
    esac
  done

  [ "${#config_paths[@]}" -gt 0 ] || return 0
  # SYSCOIN: syscoind rotates cookie credentials on restart. Keep generated
  # os-server configs aligned immediately before launching the node. Mirror the
  # Rust CLI's config parsing: repeated --config flags are allowed and each value
  # may contain ':'-delimited config files loaded in order.
  for config_entry in "${config_paths[@]}"; do
    while IFS= read -r config_path; do
      [ -n "${config_path}" ] || continue
      gl_refresh_bitcoin_da_config_from_cookie "${config_path}"
    done < <(printf '%s\n' "${config_entry}" | tr ':' '\n')
  done
}

if [ "${1:-}" = "exec-prebuilt" ]; then
  shift
  [ "${1:-}" = "--" ] || usage
  shift
  [ $# -gt 0 ] || usage

  refresh_os_server_config_credentials -- "$@"
  configure_build_context
  PREBUILT_BINARY="$(prebuilt_binary_path)"
  PREBUILT_STAMP="${PREBUILT_BINARY}.sha256"
  [ -x "${PREBUILT_BINARY}" ] || \
    gl_die "prebuilt zksync-os-server binary is missing or not executable: ${PREBUILT_BINARY}; run the deployment build step first"
  [ -f "${PREBUILT_STAMP}" ] || \
    gl_die "prebuilt zksync-os-server build stamp is missing: ${PREBUILT_STAMP}; run the deployment build step first"
  IFS= read -r STAMPED_DIGEST < "${PREBUILT_STAMP}"
  [[ "${STAMPED_DIGEST}" =~ ^[0-9a-f]{64}$ ]] || \
    gl_die "prebuilt zksync-os-server build stamp is malformed: ${PREBUILT_STAMP}"
  CURRENT_DIGEST="$(prebuilt_digest "${PREBUILT_BINARY}")"
  [ "${CURRENT_DIGEST}" = "${STAMPED_DIGEST}" ] || \
    gl_die "prebuilt zksync-os-server binary or build context does not match its stamp; run the deployment build step first"
  exec "${PREBUILT_BINARY}" "$@"
fi

BUILD_PREBUILT=false
if [ "${1:-}" = "build-prebuilt" ]; then
  shift
  [ $# -eq 0 ] || usage
  BUILD_PREBUILT=true
  PREBUILT_BINARY="$(prebuilt_binary_path)"
  PREBUILT_STAMP="${PREBUILT_BINARY}.sha256"
  # Invalidate the prior release before any checkout, rewrite, or compilation
  # can fail. The currently running process keeps its open executable, while a
  # later restart fails closed until this build publishes a new stamp.
  rm -f "${PREBUILT_STAMP}"
  set -- build --release --bin zksync-os-server
fi

refresh_os_server_config_credentials "$@"

if uses_patched_workspace; then
  configure_build_context
  ZKSYNC_OS_TAG="$(extract_zksync_os_tag)"
  ZKSYNC_OS_PATCHED_PATH="$(prepare_zksync_os_checkout "${ZKSYNC_OS_TAG}")"
  ZKSYNC_OS_PATCHED_REV="$(git -C "${ZKSYNC_OS_PATCHED_PATH}" rev-parse HEAD)"
  RUN_PATH="${GATEWAY_DIR}/.gateway-launch/zksync-os-server/${WORKSPACE_NAME}"
  if [ "${BUILD_PREBUILT}" = true ]; then
    TARGET_DIR="${GATEWAY_DIR}/.gateway-launch/target/${WORKSPACE_NAME}"
  else
    TARGET_DIR="${CARGO_TARGET_DIR:-${GATEWAY_DIR}/.gateway-launch/target/${WORKSPACE_NAME}}"
  fi
  prepare_run_workspace "${RUN_PATH}" "${ZKSYNC_OS_PATCHED_PATH}" "${ZKSYNC_OS_TAG}" "${ZKSYNC_OS_PATCHED_REV}"
  clear_multivm_build_script_cache "${TARGET_DIR}"
  cd "${RUN_PATH}"
  export CARGO_TARGET_DIR="${TARGET_DIR}"
else
  cd "${ZKSYNC_OS_SERVER_PATH}"
  if [ "${BUILD_PREBUILT}" = true ]; then
    TARGET_DIR="${GATEWAY_DIR}/.gateway-launch/target/${WORKSPACE_NAME}"
    export CARGO_TARGET_DIR="${TARGET_DIR}"
  fi
fi

if [ "${BUILD_PREBUILT}" = true ]; then
  cargo "$@"
  [ -x "${PREBUILT_BINARY}" ] || \
    gl_die "cargo build completed without an executable zksync-os-server binary: ${PREBUILT_BINARY}"
  PREBUILT_STAMP_TMP="${PREBUILT_STAMP}.tmp.$$"
  prebuilt_digest "${PREBUILT_BINARY}" > "${PREBUILT_STAMP_TMP}"
  mv "${PREBUILT_STAMP_TMP}" "${PREBUILT_STAMP}"
  exit 0
fi

cargo "$@"
