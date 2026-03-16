#!/usr/bin/env bash
set -euo pipefail

CURRENT_BIN=${CURRENT_BIN:?CURRENT_BIN is required}
LOADBASE_PATH=${LOADBASE_PATH:?LOADBASE_PATH is required}
RELEASE_DIR=${RELEASE_DIR:?RELEASE_DIR is required}
SHARED_DB=${SHARED_DB:?SHARED_DB is required}
LOG_DIR=${LOG_DIR:?LOG_DIR is required}
GITHUB_REPOSITORY=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}

PROTOCOL_VERSION=${PROTOCOL_VERSION:-v30.2}
LOCAL_CHAINS_DIR=${LOCAL_CHAINS_DIR:-local-chains}
SERVER_BIN=${SERVER_BIN:-zksync-os-server}
SERVER_PORT=${SERVER_PORT:-3050}
L1_PORT=${L1_PORT:-8545}
RICH_PRIVATE_KEY=${RICH_PRIVATE_KEY:-0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110}
LOADBASE_DURATION=${LOADBASE_DURATION:-30s}
LOADBASE_WALLETS=${LOADBASE_WALLETS:-12}
LOADBASE_MAX_IN_FLIGHT=${LOADBASE_MAX_IN_FLIGHT:-15}

mkdir -p "${RELEASE_DIR}" "${SHARED_DB}" "${LOG_DIR}"

CURRENT_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
if [[ -z "${CURRENT_VERSION}" ]]; then
  echo "Failed to determine current version from Cargo.toml"
  exit 1
fi

IFS='.' read -r CURRENT_MAJOR CURRENT_MINOR CURRENT_PATCH <<< "${CURRENT_VERSION}"
if (( CURRENT_MINOR == 0 )); then
  echo "Current version ${CURRENT_VERSION} has no previous minor line"
  exit 1
fi

PREVIOUS_MINOR=$((CURRENT_MINOR - 1))
PREVIOUS_TAG="$(
  gh api "repos/${GITHUB_REPOSITORY}/releases?per_page=100" \
    --jq '
      map(select(.draft == false and .prerelease == false))
      | map(.tag_name)
      | map(select(test("^v" + "'"${CURRENT_MAJOR}"'\\.'"${PREVIOUS_MINOR}"'\\.[0-9]+$")))
      | sort_by(ltrimstr("v") | split(".") | map(tonumber))
      | last
    '
)"

if [[ -z "${PREVIOUS_TAG}" || "${PREVIOUS_TAG}" == "null" ]]; then
  echo "Could not find a release for v${CURRENT_MAJOR}.${PREVIOUS_MINOR}.x"
  exit 1
fi

gh release download "${PREVIOUS_TAG}" \
  --repo "${GITHUB_REPOSITORY}" \
  --dir "${RELEASE_DIR}" \
  --pattern "${SERVER_BIN}-${PREVIOUS_TAG}-x86_64-unknown-linux-gnu.tar.gz"

tar -xzf "${RELEASE_DIR}/${SERVER_BIN}-${PREVIOUS_TAG}-x86_64-unknown-linux-gnu.tar.gz" -C "${RELEASE_DIR}"
PREVIOUS_BIN="${RELEASE_DIR}/${SERVER_BIN}"

L1_STATE_JSON="${LOCAL_CHAINS_DIR}/${PROTOCOL_VERSION}/l1-state.json"
gzip -dfk "${L1_STATE_JSON}.gz"

ANVIL_PID=""
SERVER_PID=""

cleanup() {
  if [[ -n "${SERVER_PID}" ]]; then
    kill "${SERVER_PID}" >/dev/null 2>&1 || true
    wait "${SERVER_PID}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${ANVIL_PID}" ]]; then
    kill "${ANVIL_PID}" >/dev/null 2>&1 || true
    wait "${ANVIL_PID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

wait_for_rpc() {
  local attempts=0
  until cast rpc --rpc-url "http://127.0.0.1:${SERVER_PORT}" eth_blockNumber >/dev/null 2>&1; do
    if [[ -n "${SERVER_PID}" ]] && ! kill -0 "${SERVER_PID}" >/dev/null 2>&1; then
      echo "Server process ${SERVER_PID} exited before RPC became ready"
      return 1
    fi
    attempts=$((attempts + 1))
    if (( attempts > 60 )); then
      echo "Timed out waiting for server RPC on port ${SERVER_PORT}"
      return 1
    fi
    sleep 2
  done
}

wait_for_anvil() {
  local attempts=0
  until cast rpc --rpc-url "http://127.0.0.1:${L1_PORT}" eth_blockNumber >/dev/null 2>&1; do
    if [[ -n "${ANVIL_PID}" ]] && ! kill -0 "${ANVIL_PID}" >/dev/null 2>&1; then
      echo "Anvil process ${ANVIL_PID} exited before RPC became ready"
      return 1
    fi
    attempts=$((attempts + 1))
    if (( attempts > 30 )); then
      echo "Timed out waiting for Anvil RPC on port ${L1_PORT}"
      return 1
    fi
    sleep 1
  done
}

hex_to_dec() {
  local value="$1"
  printf '%d\n' "$((16#${value#0x}))"
}

latest_block_number() {
  cast rpc --rpc-url "http://127.0.0.1:${SERVER_PORT}" eth_blockNumber
}

run_loadbase() {
  local phase="$1"
  local output_dir="${LOG_DIR}/${phase}-loadbase"
  mkdir -p "${output_dir}"
  "${LOADBASE_PATH}" \
    --rpc-url "http://127.0.0.1:${SERVER_PORT}" \
    --rich-privkey "${RICH_PRIVATE_KEY}" \
    --duration "${LOADBASE_DURATION}" \
    --max-in-flight "${LOADBASE_MAX_IN_FLIGHT}" \
    --wallets "${LOADBASE_WALLETS}" \
    --dest random \
    > "${output_dir}/stdout.log" 2>&1
}

assert_block_progress() {
  local phase="$1"
  local before_dec="$2"
  local after_hex
  local after_dec
  local attempts=0
  while true; do
    after_hex="$(latest_block_number)"
    after_dec="$(hex_to_dec "${after_hex}")"
    if (( after_dec > before_dec )); then
      echo "Phase ${phase}: block advanced from ${before_dec} to ${after_dec}"
      return 0
    fi
    attempts=$((attempts + 1))
    if (( attempts > 30 )); then
      echo "Phase ${phase}: block did not advance beyond ${before_dec}"
      return 1
    fi
    sleep 2
  done
}

start_server() {
  local phase="$1"
  local binary_path="$2"
  local log_path="${LOG_DIR}/${phase}.log"
  general_rocks_db_path="${SHARED_DB}" \
    "${binary_path}" \
    --config "./local-chains/${PROTOCOL_VERSION}/default/config.yaml" \
    > "${log_path}" 2>&1 &
  SERVER_PID=$!
  wait_for_rpc
  echo "Started ${phase} server with PID ${SERVER_PID}"
}

stop_server() {
  local phase="$1"
  if [[ -z "${SERVER_PID}" ]]; then
    echo "Phase ${phase}: server PID is empty"
    return 1
  fi
  kill "${SERVER_PID}"
  wait "${SERVER_PID}" || true
  SERVER_PID=""
}

run_phase() {
  local phase="$1"
  local binary_path="$2"
  local before_hex
  local before_dec

  start_server "${phase}" "${binary_path}"
  before_hex="$(latest_block_number)"
  before_dec="$(hex_to_dec "${before_hex}")"
  echo "Phase ${phase}: starting from block ${before_dec}"
  run_loadbase "${phase}"
  assert_block_progress "${phase}" "${before_dec}"
  stop_server "${phase}"
}

anvil --load-state "${L1_STATE_JSON}" --port "${L1_PORT}" > "${LOG_DIR}/anvil.log" 2>&1 &
ANVIL_PID=$!
wait_for_anvil

run_phase "latest-initial" "${CURRENT_BIN}"
run_phase "previous-minor" "${PREVIOUS_BIN}"
run_phase "latest-final" "${CURRENT_BIN}"
