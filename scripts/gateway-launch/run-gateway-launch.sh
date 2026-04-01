#!/usr/bin/env bash
# Canonical launcher for Gateway + Edge on Tanenbaum/Mainnet.
# This script runs a fixed checkpointed pipeline. No user-facing skip/with/anvil controls.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIG_ARGS=("$@")
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_validate_prover_mode

if [ -f "${HOME}/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "${HOME}/.cargo/env"
fi

if [ -z "${GATEWAY_PROVER_MODE:-}" ]; then
  if [ "${PROVER_MODE}" = "no-proofs" ]; then
    export GATEWAY_PROVER_MODE="no-proofs"
  else
    export GATEWAY_PROVER_MODE="gpu"
  fi
fi

L1_PROFILE=""

usage() {
  cat <<'EOF'
run-gateway-launch.sh --l1 tanenbaum|mainnet [--log PATH]

Required env:
  L1_RPC_URL=http(s)://...  (HTTP(S) only)

Optional env:
  GATEWAY_ARCHIVE_L1_RPC_URL   runtime L1 RPC for os-server/migration startup (defaults to L1_RPC_URL)
  PROTOCOL_VERSION             default v31.0
  GATEWAY_DIR                  default ~/gateway
  PROVER_MODE                  gpu|no-proofs (default gpu)
  GATEWAY_PROVER_MODE          ecosystem prover mode, defaults from PROVER_MODE
  GATEWAY_LAUNCH_LOG           default ~/gateway-launch.log
  BITCOIN_DA_RPC_URL / BITCOIN_DA_RPC_USER / BITCOIN_DA_RPC_PASSWORD
  GATEWAY_FUND_WALLETS_PATHS   optional extra wallets.yaml list (colon-separated)

Options:
  --l1 tanenbaum|mainnet
  --log PATH
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
  --log)
    GATEWAY_LAUNCH_LOG="${2:?}"
    shift 2
    ;;
  -h | --help) usage 0 ;;
  *)
    echo "unknown arg: $1" >&2
    usage 1
    ;;
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
  echo "required: --l1 tanenbaum|mainnet" >&2
  usage 1
}

case "${L1_PROFILE}" in
tanenbaum)
  export L1_CHAIN_ID=5700
  export L1_NETWORK=tanenbaum
  gl_require L1_RPC_URL
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:18370}"
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
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:8370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
  : "${ETH_GAS_PRICE:=1gwei}"
  : "${ETH_PRIORITY_GAS_PRICE:=1gwei}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL ETH_GAS_PRICE ETH_PRIORITY_GAS_PRICE
  ;;
*)
  gl_die "invalid --l1: ${L1_PROFILE} (supported: tanenbaum|mainnet)"
  ;;
esac

case "${L1_RPC_URL}" in
http://* | https://*) ;;
*)
  gl_die "L1_RPC_URL must be http:// or https://"
  ;;
esac

export FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION:-}"
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
export GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME:-gateway}"
export EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"

json_rpc_hex_to_dec() {
  local rpc_url="${1:?rpc url required}"
  local method="${2:?rpc method required}"
  python3 - "${rpc_url}" "${method}" <<'PY'
import json
import sys
import urllib.request

rpc_url = sys.argv[1]
method = sys.argv[2]
payload = json.dumps(
    {"jsonrpc": "2.0", "method": method, "params": [], "id": 1}
).encode("utf-8")
req = urllib.request.Request(
    rpc_url,
    data=payload,
    headers={"Content-Type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(req, timeout=3) as resp:
    body = resp.read().decode("utf-8")
obj = json.loads(body)
result = obj.get("result")
if not isinstance(result, str) or not result.startswith("0x"):
    raise SystemExit(1)
print(int(result, 16))
PY
}

wait_for_rpc() {
  local i
  for i in $(seq 1 60); do
    chain_id="$(json_rpc_hex_to_dec "${L1_RPC_URL}" "eth_chainId" 2>/dev/null || true)"
    if [ -n "${chain_id}" ]; then
      echo "L1 RPC up, chain-id ${chain_id}"
      return 0
    fi
    sleep 1
  done
  gl_die "L1 RPC not responding: ${L1_RPC_URL}"
}

gateway_rpc_ready() {
  local rpc_port block_no
  rpc_port="${GATEWAY_OS_RPC_PORT:-3052}"
  block_no="$(json_rpc_hex_to_dec "http://127.0.0.1:${rpc_port}" "eth_blockNumber" 2>/dev/null || true)"
  [ -n "${block_no}" ]
}

print_gateway_prover_mode_hint() {
  local effective_gateway_mode
  effective_gateway_mode="$(gl_to_lower "${GATEWAY_PROVER_MODE:-${PROVER_MODE}}")"
  if [ "${effective_gateway_mode}" = "gpu" ]; then
    echo "migrate-edge: Gateway prover mode is gpu; Gateway RPC up does not imply proving is active."
    echo "migrate-edge: ensure an external Gateway prover is running and connected, otherwise prove batches can stall."
  fi
}

set_gateway_runtime_l1_rpc_url() {
  local chain_name config_path migration_l1_rpc
  chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  config_path="${GATEWAY_DIR}/os-server-configs/${chain_name}/config.yaml"
  [ -f "${config_path}" ] || gl_die "missing Gateway config for migration: ${config_path}"
  migration_l1_rpc="${GATEWAY_ARCHIVE_L1_RPC_URL:-${L1_RPC_URL:-}}"
  [ -n "${migration_l1_rpc}" ] || gl_die "missing runtime archive L1 RPC URL"

  python3 - "${config_path}" "${migration_l1_rpc}" <<'PY'
import json
import re
import sys
from pathlib import Path

config_path = Path(sys.argv[1])
new_rpc_url = sys.argv[2]
text = config_path.read_text(encoding="utf-8")
updated, n = re.subn(r"^(\s*l1_rpc_url:\s*).*$", lambda m: f'{m.group(1)}{json.dumps(new_rpc_url)}', text, count=1, flags=re.MULTILINE)
if n != 1:
    raise SystemExit(f"failed to patch l1_rpc_url in {config_path}")
if updated != text:
    config_path.write_text(updated, encoding="utf-8")
print(f"gateway-launch: set {config_path} l1_rpc_url -> {new_rpc_url}")
PY
}

GATEWAY_NODE_PID=""
GATEWAY_STARTED_FOR_MIGRATION=false

start_gateway_for_migration() {
  local start_script log_file i start_timeout_s poll_interval_s max_checks chain_name
  chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  start_script="${GATEWAY_DIR}/os-server-configs/${chain_name}/start-node.sh"
  [ -x "${start_script}" ] || gl_die "missing executable Gateway start script: ${start_script}"
  set_gateway_runtime_l1_rpc_url

  if gateway_rpc_ready; then
    echo "migrate-edge: Gateway RPC already reachable; reusing running node"
    print_gateway_prover_mode_hint
    return 0
  fi

  : "${GATEWAY_MIGRATION_GATEWAY_LOG:=${HOME}/gateway-migration-gateway-node.log}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT:=3600}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_POLL:=2}"
  start_timeout_s="${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT}"
  poll_interval_s="${GATEWAY_MIGRATION_GATEWAY_START_POLL}"
  [ "${poll_interval_s}" -gt 0 ] || poll_interval_s=2
  max_checks=$((start_timeout_s / poll_interval_s))
  [ "${max_checks}" -gt 0 ] || max_checks=1

  log_file="${GATEWAY_MIGRATION_GATEWAY_LOG}"
  echo "migrate-edge: starting Gateway node via ${start_script} -> ${log_file}"
  nohup bash "${start_script}" >"${log_file}" 2>&1 &
  GATEWAY_NODE_PID=$!
  GATEWAY_STARTED_FOR_MIGRATION=true

  print_gateway_migration_log_excerpt() {
    local file_path="${1:?log path required}"
    [ -f "${file_path}" ] || {
      echo "migrate-edge: log file not found: ${file_path}" >&2
      return 0
    }
    python3 - "${file_path}" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8", errors="replace").splitlines()
tail = text[-120:] if len(text) > 120 else text
print("migrate-edge: Gateway node log excerpt (last {} lines):".format(len(tail)), file=sys.stderr)
for line in tail:
    print(line, file=sys.stderr)
PY
  }

  gateway_replay_assertion_failed() {
    local file_path="${1:?log path required}"
    [ -f "${file_path}" ] || return 1
    python3 - "${file_path}" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
patterns = (
    r"assertion `left == right` failed",
    r"PanickedTaskError",
    r"task_name:\s*\"block_executor\"",
)
ok = all(re.search(p, text) for p in patterns)
raise SystemExit(0 if ok else 1)
PY
  }

  for i in $(seq 1 "${max_checks}"); do
    if gateway_rpc_ready; then
      echo "migrate-edge: Gateway RPC is up"
      print_gateway_prover_mode_hint
      return 0
    fi
    if ! kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null; then
      print_gateway_migration_log_excerpt "${log_file}"
      if gateway_replay_assertion_failed "${log_file}"; then
        gl_die "migrate-edge: Gateway node failed during replay with block_executor assertion mismatch. This usually means stale gateway DB state from a prior incompatible run. Remove ${GATEWAY_DIR}/os-server-configs/${chain_name}/db and rerun."
      fi
      gl_die "migrate-edge: Gateway node exited before RPC came up; see ${log_file}"
    fi
    sleep "${poll_interval_s}"
  done
  print_gateway_migration_log_excerpt "${log_file}"
  gl_die "migrate-edge: Gateway RPC did not come up within ${start_timeout_s}s (see ${log_file})"
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

run_migrate_edge_with_retry() {
  local attempt max_attempts status migrate_output
  local migrate_output_lc
  max_attempts="${GATEWAY_MIGRATE_EDGE_MAX_ATTEMPTS:-2}"
  [ "${max_attempts}" -gt 0 ] || max_attempts=1
  for attempt in $(seq 1 "${max_attempts}"); do
    set +e
    migrate_output="$("${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" 2>&1)"
    status=$?
    set -e
    echo "${migrate_output}"
    if [ "${status}" -eq 0 ]; then
      return 0
    fi
    if [ "${attempt}" -ge "${max_attempts}" ]; then
      return "${status}"
    fi
    migrate_output_lc="$(gl_to_lower "${migrate_output}")"
    if [[ "${migrate_output_lc}" == *"insufficient funds for transfer"* ]]; then
      echo "migrate-edge: insufficient funds detected; topping up and retrying"
      "${SCRIPT_DIR}/fund-wallets.sh"
      continue
    fi
    return "${status}"
  done
}

cleanup() {
  stop_gateway_for_migration
}
trap cleanup EXIT INT TERM

checkpoint_should_skip() {
  local checkpoint_id="${1:?checkpoint id required}"
  shift
  local status
  status="$(gl_checkpoint_get_status "${checkpoint_id}")"
  if [ "${status}" != "passed" ]; then
    return 1
  fi
  "$@"
}

run_checkpoint_with_validation() {
  local checkpoint_id="${1:?checkpoint id required}"
  local validator_fn="${2:?validator function required}"
  shift 2

  if checkpoint_should_skip "${checkpoint_id}" "${validator_fn}"; then
    echo "checkpoint ${checkpoint_id} already passed; skipping"
    return 0
  fi

  gl_checkpoint_run "${checkpoint_id}" "$@" || return $?
  if ! "${validator_fn}"; then
    gl_checkpoint_mark_blocked "${checkpoint_id}" "post-run validation failed"
    gl_die "checkpoint ${checkpoint_id} validation failed after command success"
  fi
}

validate_workspace() { gl_probe_workspace_ready; }
validate_ecosystem() { gl_probe_ecosystem_ready; }
validate_wallets_funded() { gl_probe_wallets_funded_ready; }
validate_l1_deployed() { gl_probe_l1_ecosystem_deployed_ready; }
validate_gateway_chain_inited() { gl_probe_gateway_chain_inited_ready; }
validate_gateway_settlement() { gl_probe_gateway_settlement_ready; }
validate_os_configs_gateway() { gl_probe_os_configs_gateway_ready; }
validate_edge_chain_inited() { gl_probe_edge_chain_inited_ready; }
validate_migration() { return 1; }
validate_os_configs_final() { gl_probe_os_configs_final_ready; }

step_workspace() {
  wait_for_rpc
  gl_l1_broadcast_preflight
}

step_ecosystem() {
  if [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
    echo "gateway-launch: reusing existing ecosystem at ${GATEWAY_DIR}"
  else
    "${SCRIPT_DIR}/gateway-ecosystem-create.sh"
  fi
  gl_resolve_gateway_dir_after_ecosystem_create
}

step_l1_ecosystem_deployed() {
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  # A fresh L1 ecosystem deployment can produce different replay envelopes.
  # Clear stale runtime DB so gateway node does not panic on replay mismatch.
  gl_clear_os_server_chain_db "${gateway_chain_name}"
  "${SCRIPT_DIR}/gateway-deploy-l1.sh"
}

step_edge_chain_inited() {
  local edge_chain_name
  edge_chain_name="${EDGE_CHAIN_NAME:-zksys}"
  # Edge chain redeploy/init can also invalidate previously replayed runtime state.
  gl_clear_os_server_chain_db "${edge_chain_name}"
  "${SCRIPT_DIR}/edge-chain-create-init.sh"
}

echo "gateway-launch: initializing checkpoint state"
gl_checkpoint_state_init
wait_for_rpc
gl_ensure_zksync_era_workspace
if [ ! -x "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack" ]; then
  echo "gateway-launch: building zkstack CLI (first use for this pin)"
  gl_build_zkstack_cli_release
fi
gl_path_for_zkstack
gl_checkpoint_set_fingerprint_if_empty
gl_checkpoint_assert_fingerprint_matches

run_checkpoint_with_validation "gl.workspace" validate_workspace step_workspace || exit $?
run_checkpoint_with_validation "gl.ecosystem" validate_ecosystem step_ecosystem || exit $?
run_checkpoint_with_validation "gl.wallets_funded" validate_wallets_funded "${SCRIPT_DIR}/fund-wallets.sh" || exit $?
run_checkpoint_with_validation "gl.l1_ecosystem_deployed" validate_l1_deployed step_l1_ecosystem_deployed || exit $?
run_checkpoint_with_validation "gl.gateway_chain_inited" validate_gateway_chain_inited "${SCRIPT_DIR}/gateway-chain-init.sh" || exit $?
run_checkpoint_with_validation "gl.gateway_settlement" validate_gateway_settlement "${SCRIPT_DIR}/gateway-convert-settlement.sh" || exit $?
run_checkpoint_with_validation "gl.os_configs_gateway" validate_os_configs_gateway env MATERIALIZE_EDGE_CONFIG=false "${SCRIPT_DIR}/generate-os-server-configs.sh" || exit $?
run_checkpoint_with_validation "gl.edge_chain_inited" validate_edge_chain_inited step_edge_chain_inited || exit $?

if checkpoint_should_skip "gl.migration" validate_migration; then
  echo "checkpoint gl.migration already passed; skipping"
else
  gl_checkpoint_mark_in_progress "gl.migration"
  set +e
  start_gateway_for_migration
  run_migrate_edge_with_retry
  migration_rc=$?
  stop_gateway_for_migration
  set -e
  if [ "${migration_rc}" -ne 0 ]; then
    gl_checkpoint_mark_blocked "gl.migration" "migration failed with exit code ${migration_rc}"
    exit "${migration_rc}"
  fi
  gl_checkpoint_mark_passed "gl.migration"
fi

run_checkpoint_with_validation "gl.os_configs_final" validate_os_configs_final "${SCRIPT_DIR}/generate-os-server-configs.sh" || exit $?

echo "=== gateway-launch complete ==="
trap - EXIT INT TERM
