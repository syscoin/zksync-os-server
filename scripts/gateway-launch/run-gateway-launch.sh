#!/usr/bin/env bash
# Single entry: Gateway ecosystem + L1 deploy + chain init + convert-to-gateway (+ optional edge chain / migrate).
#
# Usage:
#   export ZKSYNC_ERA_PATH=/path/to/zksync-era
#   export ZKSYNC_OS_SERVER_PATH=/path/to/zksync-os-server   # optional; defaults to this repo
#   export L1_RPC_URL=https://...                           # required for tanenbaum | mainnet
#   bash run-gateway-launch.sh --l1 anvil [options]
#
# Profiles (--l1):
#   anvil       chain 9, L1_NETWORK=localhost, default L1_RPC_URL=http://127.0.0.1:8545
#   tanenbaum   chain 5700, L1_NETWORK=tanenbaum, L1_RPC_URL required
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
#   --migrate-edge       run edge-chain-migrate-to-gateway.sh (Gateway L2 RPC must be reachable)
#   --log PATH           tee stdout/stderr here (default: ~/gateway-launch.log)
#   -h, --help
#
# Env: PROTOCOL_VERSION, GATEWAY_DIR, GATEWAY_ECOSYSTEM_PARENT_DIR, EDGE_CHAIN_NAME, EDGE_CHAIN_ID,
#      FUNDER_PRIVATE_KEY (for fund; Anvil default is dev key 0), FOUNDRY_EVM_VERSION=shanghai
#
# nohup: outer re-exec under `script` is not enough — `exec > >(tee log)` makes stdout a pipe for zkstack.
# `gl_zkstack_pty` in gateway-chain-init.sh wraps `zkstack chain init` with util-linux `script`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIG_ARGS=("$@")
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

L1_PROFILE=""
START_ANVIL=true
START_WATCH=true
REUSE_ECOSYSTEM=false
RESET_L1=false
SKIP_FUND=false
STOP_AFTER_L1=false
WITH_EDGE=false
MIGRATE_EDGE=false

usage() {
  cat <<'EOF'
run-gateway-launch.sh --l1 anvil|tanenbaum|mainnet [options]

Required env:
  ZKSYNC_ERA_PATH=/path/to/zksync-era
  L1_RPC_URL=...          (required for tanenbaum and mainnet)

Optional env:
  ZKSYNC_OS_SERVER_PATH   defaults to zksync-os-server repo root
  PROTOCOL_VERSION        default v31.0
  GATEWAY_DIR             default ~/gateway
  FUNDER_PRIVATE_KEY      for fund-wallets (Anvil defaults to dev key 0)
  GATEWAY_LAUNCH_LOG      default ~/gateway-launch.log

Options:
  --no-start-anvil        (anvil) Anvil already running
  --no-start-watch        (anvil) skip background txpool miner
  --reuse-ecosystem       skip ecosystem create
  --reset-l1-artifacts    rm broadcast/script-out before L1
  --skip-fund             wallets already funded
  --stop-after-l1         skip chain init + convert (+ edge)
  --with-edge             create+init edge chain after gateway
  --migrate-edge          migrate edge to gateway (needs Gateway L2 up)
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

[ -n "${L1_PROFILE}" ] || {
  echo "required: --l1 anvil|tanenbaum|mainnet" >&2
  usage 1
}

gl_require ZKSYNC_ERA_PATH

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
  ;;
mainnet)
  export L1_CHAIN_ID=57
  export L1_NETWORK=mainnet
  gl_require L1_RPC_URL
  ;;
*)
  echo "invalid --l1: ${L1_PROFILE}" >&2
  exit 1
  ;;
esac

export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-shanghai}"
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
gl_assert_contracts_sha
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
cleanup() {
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
  (
    export L1_RPC_URL PATH
    while pgrep -f 'DeployL1CoreContracts\.s\.sol|DeployCTM\.s\.sol|zkstack ecosystem init|zkstack chain init' >/dev/null 2>&1; do
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
      sleep 1
    done
  ) &
  WATCH_PID=$!
fi

if [ "${REUSE_ECOSYSTEM}" = true ]; then
  test -f "${GATEWAY_DIR}/ZkStack.yaml" || gl_die "reuse-ecosystem: missing ${GATEWAY_DIR}/ZkStack.yaml"
  echo "reusing ecosystem at ${GATEWAY_DIR}"
else
  "${SCRIPT_DIR}/gateway-ecosystem-create.sh"
fi

if [ "${SKIP_FUND}" = false ]; then
  "${SCRIPT_DIR}/fund-wallets.sh"
fi

if [ "${RESET_L1}" = true ]; then
  rm -rf "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/broadcast" "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
  mkdir -p "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
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

if [ "${WITH_EDGE}" = true ]; then
  "${SCRIPT_DIR}/edge-chain-create-init.sh"
fi

if [ "${MIGRATE_EDGE}" = true ]; then
  echo "migrate-edge: ensure Gateway zksync-os-server RPC is up if required"
  "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh"
fi

echo "=== gateway-launch complete ==="

# Keep Anvil + watch running for dev unless we started anvil and user wants cleanup.
# If we started anvil, disown so trap does not kill on normal exit — actually user wants one-shot to finish and leave anvil running.
WATCH_PID=""
ANVIL_PID=""
trap - EXIT INT TERM
