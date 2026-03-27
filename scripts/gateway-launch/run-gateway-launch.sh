#!/usr/bin/env bash
# Single entry: Gateway ecosystem + L1 deploy + chain init + convert-to-gateway (+ optional edge chain / migrate).
#
# Usage:
#   (from zksync-os-server clone) ZKSYNC_OS_SERVER_PATH defaults to this repo — zksync-era is cloned/pinned automatically if ZKSYNC_ERA_PATH is unset
#   Optional: export ZKSYNC_ERA_PATH=/path/to/zksync-era
#   export L1_RPC_URL=http://127.0.0.1:8545                 # required for tanenbaum | mainnet — HTTP(S) only
#     (Foundry cast/forge; IPC/unix not supported). Local Tanenbaum: sysgeth --http — see gateway_launch.md
#   bash run-gateway-launch.sh --l1 anvil [options]
#
# Profiles (--l1):
#   anvil       chain 9, L1_NETWORK=localhost, default L1_RPC_URL=http://127.0.0.1:8545
#   tanenbaum   chain 5700, L1_NETWORK=tanenbaum, L1_RPC_URL=http(s):// required (not IPC)
#   mainnet     chain 57, L1_NETWORK=mainnet, L1_RPC_URL required
#
# Options:
#   --no-start-anvil     (anvil only) do not spawn Anvil in background
#   --no-start-watch     (anvil only) do not spawn txpool watch in background
#   --reuse-ecosystem    skip ecosystem create; use existing GATEWAY_DIR (default ~/gateway)
#   --reset-l1-artifacts rm l1-contracts/broadcast + script-out before L1 deploy (after fresh chain)
#   --skip-fund          skip fund-wallets.sh (you funded manually)
#   --stop-after-l1      stop after ecosystem init / L1 deploy (skip chain init + convert)
#   --with-edge          run edge-chain-create-init.sh after gateway steps
#   --migrate-edge       run edge-chain-migrate-to-gateway.sh (launcher starts Gateway via generated
#                        config, runs migrate/finalize, then stops Gateway; keep edge node stopped)
#   --log PATH           tee stdout/stderr here (default: ~/gateway-launch.log)
#   -h, --help
#
# Env: ZKSYNC_ERA_PATH (optional), ZKSYNC_ERA_GIT_URL, ZKSYNC_ERA_CACHE_ROOT, PROTOCOL_VERSION, GATEWAY_DIR,
#      GATEWAY_ECOSYSTEM_PARENT_DIR, EDGE_CHAIN_NAME, EDGE_CHAIN_ID, FUNDER_PRIVATE_KEY, FOUNDRY_EVM_VERSION,
#      REQUIRED_CONTRACTS_SHA, REQUIRED_ZKSTACK_CLI_SHA, BITCOIN_DA_RPC_URL, BITCOIN_DA_RPC_USER,
#      BITCOIN_DA_RPC_PASSWORD, PROVER_MODE, GATEWAY_WALLET_CREATION, GATEWAY_WALLET_PATH,
#      GATEWAY_CREATE2_FACTORY_SALT
#
# nohup: outer re-exec under `script` is not enough — `exec > >(tee log)` makes stdout a pipe for zkstack.
# `gl_zkstack_pty` in gateway-chain-init.sh wraps `zkstack chain init` with util-linux `script`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIG_ARGS=("$@")
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_validate_prover_mode

L1_PROFILE=""
START_ANVIL=true
START_WATCH=true
REUSE_ECOSYSTEM=false
RESET_L1=false
SKIP_FUND=false
STOP_AFTER_L1=false
WITH_EDGE=true
MIGRATE_EDGE=true

usage() {
  cat <<'EOF'
run-gateway-launch.sh --l1 anvil|tanenbaum|mainnet [options]

Required env:
  L1_RPC_URL=http(s)://…  (required for tanenbaum and mainnet; HTTP(S) JSON-RPC only, not IPC)

Optional env:
  ZKSYNC_OS_SERVER_PATH   defaults to zksync-os-server repo root (directory containing scripts/gateway-launch/)
  ZKSYNC_ERA_PATH         override; if unset, zksync-era is git-cloned under ~/.cache/zksync-gateway-era/…
  ZKSYNC_ERA_GIT_URL      default https://github.com/matter-labs/zksync-era.git
  ZKSYNC_ERA_CACHE_ROOT   default ~/.cache/zksync-gateway-era
  PROTOCOL_VERSION        default v31.0
  GATEWAY_DIR             default ~/gateway
  GATEWAY_WALLET_CREATION default in-file when GATEWAY_WALLET_PATH exists, else random
  GATEWAY_WALLET_PATH     default ${GATEWAY_DIR}.wallets.yaml; reused on fresh creates after first wallet generation
  GATEWAY_CREATE2_FACTORY_SALT optional uint256 override for initial_deployments create2_factory_salt
  FUNDER_PRIVATE_KEY      for fund-wallets (Anvil defaults to dev key 0)
  BITCOIN_DA_RPC_URL      Syscoin NEVM RPC for the generated gateway OS-server config (required for blob mode)
  BITCOIN_DA_RPC_USER     Syscoin NEVM RPC auth user for the generated gateway OS-server config
                        If using local Syscoin cookie auth, e.g.:
                          COOKIE="$(< ~/.syscoin/testnet3/.cookie)"
                          export BITCOIN_DA_RPC_USER="${COOKIE%%:*}"
  BITCOIN_DA_RPC_PASSWORD Syscoin NEVM RPC auth password for the generated gateway OS-server config
                        If using local Syscoin cookie auth, e.g.:
                          export BITCOIN_DA_RPC_PASSWORD="${COOKIE#*:}"
  PROVER_MODE            prover mode for generated OS-server config: gpu (default) or mock
  GATEWAY_LAUNCH_LOG      default ~/gateway-launch.log

Options:
  --no-start-anvil        (anvil) Anvil already running
  --no-start-watch        (anvil) skip background txpool miner
  --reuse-ecosystem       skip ecosystem create
  --reset-l1-artifacts    rm broadcast/script-out before L1
  --skip-fund             wallets already funded
  --stop-after-l1         skip chain init + convert (+ edge)
  --with-edge             create+init edge chain after gateway
  --migrate-edge          migrate edge to gateway (launcher starts/stops Gateway; edge node stays stopped)
  --log PATH              tee output here
  -h, --help
EOF
  exit "${1:-0}"
}

while [ "${1:-}" != "" ]; do
  case "$1" in
  --l1)
    L1_PROFILE="${2:?}"
    shift 2
    ;;
  --no-start-anvil) START_ANVIL=false; shift ;;
  --no-start-watch) START_WATCH=false; shift ;;
  --reuse-ecosystem) REUSE_ECOSYSTEM=true; shift ;;
  --reset-l1-artifacts) RESET_L1=true; shift ;;
  --skip-fund) SKIP_FUND=true; shift ;;
  --stop-after-l1) STOP_AFTER_L1=true; shift ;;
  --with-edge) WITH_EDGE=true; shift ;;
  --migrate-edge) MIGRATE_EDGE=true; shift ;;
  --log)
    GATEWAY_LAUNCH_LOG="${2:?}"
    shift 2
    ;;
  -h | --help) usage 0 ;;
  *) echo "unknown arg: $1" >&2; usage 1 ;;
  esac
done

if [[ -z "${GATEWAY_LAUNCH_IN_SCRIPT:-}" && ( ! -t 0 || ! -t 1 ) ]]; then
  export GATEWAY_LAUNCH_IN_SCRIPT=1
  _q=("$SCRIPT_DIR/run-gateway-launch.sh" "${ORIG_ARGS[@]}")
  exec script -q -c "$(printf '%q ' "${_q[@]}")" /dev/null
fi

: "${GATEWAY_LAUNCH_LOG:=${HOME}/gateway-launch.log}"
exec > >(tee "${GATEWAY_LAUNCH_LOG}") 2>&1

echo "=== gateway-launch log: ${GATEWAY_LAUNCH_LOG} ==="
echo "gateway-launch: PROVER_MODE=${PROVER_MODE}"

[ -n "${L1_PROFILE}" ] || {
  echo "required: --l1 anvil|tanenbaum|mainnet" >&2
  usage 1
}

case "${L1_PROFILE}" in
anvil)
  export L1_CHAIN_ID=9
  export L1_RPC_URL="${L1_RPC_URL:-http://127.0.0.1:8545}"
  export L1_NETWORK=localhost
  ;;
tanenbaum)
  export L1_CHAIN_ID=5700
  export L1_NETWORK=tanenbaum
  gl_require L1_RPC_URL
  # Tanenbaum: no chainlocks on the DA path; use PoW confirmation-based finality unless overridden.
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:18370}"
  if [ -z "${BITCOIN_DA_RPC_USER:-}" ] || [ -z "${BITCOIN_DA_RPC_PASSWORD:-}" ]; then
    if [ -f "${HOME}/.syscoin/testnet3/.cookie" ]; then
      COOKIE="$(< "${HOME}/.syscoin/testnet3/.cookie")"
      : "${BITCOIN_DA_RPC_USER:=${COOKIE%%:*}}"
      : "${BITCOIN_DA_RPC_PASSWORD:=${COOKIE#*:}}"
    fi
  fi
  : "${BITCOIN_DA_FINALITY_MODE:=Confirmations}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.tanenbaum.io}"
  : "${ETH_GAS_PRICE:=1gwei}"
  : "${ETH_PRIORITY_GAS_PRICE:=1gwei}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL ETH_GAS_PRICE ETH_PRIORITY_GAS_PRICE
  ;;
mainnet)
  export L1_CHAIN_ID=57
  export L1_NETWORK=mainnet
  gl_require L1_RPC_URL
  : "${ETH_GAS_PRICE:=1gwei}"
  : "${ETH_PRIORITY_GAS_PRICE:=1gwei}"
  export ETH_GAS_PRICE ETH_PRIORITY_GAS_PRICE
  ;;
  *)
  echo "invalid --l1: ${L1_PROFILE}" >&2
  exit 1
  ;;
esac

if [ "${L1_PROFILE}" != anvil ]; then
  case "${L1_RPC_URL}" in
  http://* | https://*) ;;
  *)
    gl_die "L1_RPC_URL must be http:// or https:// (Foundry cast/forge do not support IPC/unix). For local Tanenbaum, run sysgeth with --http and set e.g. http://127.0.0.1:8545. See docs/src/guides/gateway_launch.md"
    ;;
  esac
fi

export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-shanghai}"
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_ensure_zksync_era_workspace
if [ ! -x "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack" ]; then
  echo "gateway-launch: building zkstack CLI (first use for this pin)"
  gl_build_zkstack_cli_release
fi
gl_path_for_zkstack

wait_for_rpc() {
  local i
  for i in $(seq 1 40); do
    if cast chain-id --rpc-url "${L1_RPC_URL}" >/dev/null 2>&1; then
      local cid
      cid="$(cast chain-id --rpc-url "${L1_RPC_URL}" 2>/dev/null || true)"
      echo "L1 RPC up, chain-id ${cid}"
      return 0
    fi
    sleep 0.5
  done
  gl_die "L1 RPC not responding: ${L1_RPC_URL}"
}

ANVIL_PID=""
WATCH_PID=""
GATEWAY_NODE_PID=""
GATEWAY_STARTED_FOR_MIGRATION=false

gateway_rpc_ready() {
  local rpc_port
  rpc_port="${GATEWAY_OS_RPC_PORT:-3052}"
  cast block-number --rpc-url "http://127.0.0.1:${rpc_port}" >/dev/null 2>&1
}

start_gateway_for_migration() {
  local start_script log_file i
  start_script="${GATEWAY_DIR}/os-server-configs/${GATEWAY_CHAIN_NAME}/start-node.sh"
  [ -x "${start_script}" ] || gl_die "missing executable Gateway start script: ${start_script}"

  if gateway_rpc_ready; then
    echo "migrate-edge: Gateway RPC already reachable; reusing running node"
    return 0
  fi

  : "${GATEWAY_MIGRATION_GATEWAY_LOG:=${HOME}/gateway-migration-gateway-node.log}"
  log_file="${GATEWAY_MIGRATION_GATEWAY_LOG}"
  echo "migrate-edge: starting Gateway node via ${start_script} -> ${log_file}"
  nohup bash "${start_script}" >"${log_file}" 2>&1 &
  GATEWAY_NODE_PID=$!
  GATEWAY_STARTED_FOR_MIGRATION=true

  for i in $(seq 1 150); do
    if gateway_rpc_ready; then
      echo "migrate-edge: Gateway RPC is up"
      return 0
    fi
    if ! kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null; then
      break
    fi
    sleep 2
  done

  gl_die "migrate-edge: Gateway RPC did not come up (see ${log_file})"
}

stop_gateway_for_migration() {
  if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ] && [ -n "${GATEWAY_NODE_PID}" ]; then
    echo "migrate-edge: stopping Gateway node (pid ${GATEWAY_NODE_PID})"
    kill "${GATEWAY_NODE_PID}" 2>/dev/null || true
    wait "${GATEWAY_NODE_PID}" 2>/dev/null || true
  fi
  GATEWAY_NODE_PID=""
  GATEWAY_STARTED_FOR_MIGRATION=false
}

cleanup() {
  stop_gateway_for_migration
  if [ -n "${WATCH_PID}" ]; then
    kill "${WATCH_PID}" 2>/dev/null || true
  fi
  if [ -n "${ANVIL_PID}" ]; then
    kill "${ANVIL_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

if [ "${L1_PROFILE}" = anvil ] && [ "${START_ANVIL}" = true ]; then
  : "${GATEWAY_LOCAL_ANVIL_LOG:=${HOME}/gateway-local-anvil.log}"
  : >"${GATEWAY_LOCAL_ANVIL_LOG}"
  echo "starting Anvil -> ${GATEWAY_LOCAL_ANVIL_LOG}"
  nohup "${SCRIPT_DIR}/anvil-local-start.sh" >>"${GATEWAY_LOCAL_ANVIL_LOG}" 2>&1 &
  ANVIL_PID=$!
  wait_for_rpc
  cid="$(cast chain-id --rpc-url "${L1_RPC_URL}")"
  [ "$cid" = "9" ] || gl_die "expected anvil chain id 9, got ${cid}"
elif [ "${L1_PROFILE}" != anvil ]; then
  wait_for_rpc
else
  wait_for_rpc
fi

if [ "${L1_PROFILE}" = anvil ] && [ "${START_WATCH}" = true ]; then
  : "${GATEWAY_LOCAL_ANVIL_WATCH_LOG:=${HOME}/gateway-local-anvil-watch.log}"
  : >"${GATEWAY_LOCAL_ANVIL_WATCH_LOG}"
  echo "starting txpool watch -> ${GATEWAY_LOCAL_ANVIL_WATCH_LOG}"
  LAUNCHER_PID=$$
  (
    export L1_RPC_URL PATH
    while kill -0 "${LAUNCHER_PID}" 2>/dev/null; do
      if pgrep -f 'DeployL1CoreContracts\.s\.sol|DeployCTM\.s\.sol|zkstack ecosystem init|zkstack chain init' >/dev/null 2>&1; then
        # txpool_status "pending" vs "queued" (foundry#10122); also mine every tick — forge can stall with
        # pending=queued=0 while run-latest has 0 receipts; txpool-only watch never fires.
        _tp="$(cast rpc txpool_status --rpc-url "${L1_RPC_URL}" 2>/dev/null || echo '{}')"
        read -r P Q <<EOF
$(python3 -c "import json,sys; j=json.loads(sys.argv[1]); print(int(j.get('pending','0x0'),16), int(j.get('queued','0x0'),16))" "${_tp}")
EOF
        if [ "${P}" != "0" ] || [ "${Q}" != "0" ]; then
          echo "[watch] $(date -Is) pending=${P} queued=${Q}" >>"${GATEWAY_LOCAL_ANVIL_WATCH_LOG}"
        fi
        cast rpc anvil_mine 1 --rpc-url "${L1_RPC_URL}" >/dev/null || true
      fi
      sleep 1
    done
  ) &
  WATCH_PID=$!
fi

if [ "${REUSE_ECOSYSTEM}" = false ] && [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
  REUSE_ECOSYSTEM=true
  echo "gateway-launch: existing ecosystem detected at ${GATEWAY_DIR}; auto-enabling --reuse-ecosystem"
fi

if [ "${REUSE_ECOSYSTEM}" = true ]; then
  :
else
  "${SCRIPT_DIR}/gateway-ecosystem-create.sh"
fi
gl_resolve_gateway_dir_after_ecosystem_create
if [ "${REUSE_ECOSYSTEM}" = true ]; then
  echo "reusing ecosystem at ${GATEWAY_DIR}"
fi

if [ "${SKIP_FUND}" = false ]; then
  "${SCRIPT_DIR}/fund-wallets.sh"
fi

if [ "${RESET_L1}" = true ]; then
  rm -rf "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/broadcast" "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
  mkdir -p "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
  touch "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out/.gitkeep"
fi

"${SCRIPT_DIR}/gateway-deploy-l1.sh"

if [ "${STOP_AFTER_L1}" = true ]; then
  echo "=== stop-after-l1: done ==="
  [ -n "${WATCH_PID}" ] && kill "${WATCH_PID}" 2>/dev/null || true
  WATCH_PID=""
  ANVIL_PID=""
  trap - EXIT INT TERM
  exit 0
fi

"${SCRIPT_DIR}/gateway-chain-init.sh"
"${SCRIPT_DIR}/gateway-convert-settlement.sh"
"${SCRIPT_DIR}/generate-os-server-configs.sh"

if [ "${WITH_EDGE}" = true ]; then
  SKIP_FUND="${SKIP_FUND}" "${SCRIPT_DIR}/edge-chain-create-init.sh"
  "${SCRIPT_DIR}/generate-os-server-configs.sh"
fi

if [ "${MIGRATE_EDGE}" = true ]; then
  echo "migrate-edge: running Gateway start -> migrate/finalize -> Gateway stop"
  start_gateway_for_migration
  "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh"
  stop_gateway_for_migration
fi

echo "=== gateway-launch complete ==="

# Keep Anvil + watch running for dev unless we started anvil and user wants cleanup.
# If we started anvil, disown so trap does not kill on normal exit — actually user wants one-shot to finish and leave anvil running.
WATCH_PID=""
ANVIL_PID=""
trap - EXIT INT TERM
