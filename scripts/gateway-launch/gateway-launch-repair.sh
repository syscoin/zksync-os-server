#!/usr/bin/env bash
# Explicit checkpoint repair helper for run-gateway-launch.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_validate_prover_mode

if [ -z "${GATEWAY_PROVER_MODE:-}" ]; then
  if [ "${PROVER_MODE}" = "no-proofs" ]; then
    export GATEWAY_PROVER_MODE="no-proofs"
  else
    export GATEWAY_PROVER_MODE="gpu"
  fi
fi

L1_PROFILE=""
COMMAND=""
CHECKPOINT_ID=""

usage() {
  cat <<'EOF'
gateway-launch-repair.sh --l1 tanenbaum|mainnet status
gateway-launch-repair.sh --l1 tanenbaum|mainnet repair <checkpoint-id>

Checkpoints:
  gl.workspace
  gl.ecosystem
  gl.wallets_funded
  gl.l1_ecosystem_deployed
  gl.gateway_chain_inited
  gl.gateway_settlement
  gl.os_configs_gateway
  gl.edge_chain_inited
  gl.migration
  gl.os_configs_final
EOF
  exit "${1:-0}"
}

while [ "${1:-}" != "" ]; do
  case "$1" in
  --l1)
    L1_PROFILE="${2:?}"
    shift 2
    ;;
  status)
    COMMAND="status"
    shift
    ;;
  repair)
    COMMAND="repair"
    CHECKPOINT_ID="${2:?checkpoint id required}"
    shift 2
    ;;
  -h | --help)
    usage 0
    ;;
  *)
    echo "unknown arg: $1" >&2
    usage 1
    ;;
  esac
done

[ -n "${COMMAND}" ] || usage 1

[ -n "${L1_PROFILE}" ] || gl_die "required: --l1 tanenbaum|mainnet"

case "${L1_PROFILE}" in
tanenbaum)
  export L1_CHAIN_ID=5700
  export L1_NETWORK=tanenbaum
  gl_require L1_RPC_URL
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:18370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Confirmations}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.tanenbaum.io}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL
  ;;
mainnet)
  export L1_CHAIN_ID=57
  export L1_NETWORK=mainnet
  gl_require L1_RPC_URL
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:8370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL
  ;;
*)
  gl_die "invalid --l1: ${L1_PROFILE}"
  ;;
esac

case "${L1_RPC_URL}" in
http://* | https://*) ;;
*) gl_die "L1_RPC_URL must be http:// or https://" ;;
esac

gl_export_foundry_evm_version
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
export GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME:-gateway}"
export EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_CONTRACTS_SHA="${REQUIRED_CONTRACTS_SHA:-$(gl_contracts_sha_from_versions)}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"

gl_ensure_zksync_era_workspace
if [ ! -x "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack" ]; then
  gl_build_zkstack_cli_release
fi
gl_path_for_zkstack

gl_checkpoint_state_init
gl_checkpoint_set_fingerprint_if_empty
gl_checkpoint_assert_fingerprint_matches

checkpoint_is_known() {
  case "${1}" in
  gl.workspace | gl.ecosystem | gl.wallets_funded | gl.l1_ecosystem_deployed | gl.gateway_chain_inited | gl.gateway_settlement | gl.os_configs_gateway | gl.edge_chain_inited | gl.migration | gl.os_configs_final) return 0 ;;
  *) return 1 ;;
  esac
}

validate_checkpoint() {
  local checkpoint_id="${1:?checkpoint id required}"
  case "${checkpoint_id}" in
  gl.workspace)
    gl_probe_workspace_ready && gl_l1_broadcast_preflight
    ;;
  gl.ecosystem)
    gl_probe_ecosystem_ready
    ;;
  gl.wallets_funded)
    gl_probe_wallets_funded_ready
    ;;
  gl.l1_ecosystem_deployed)
    gl_probe_l1_ecosystem_deployed_ready
    ;;
  gl.gateway_chain_inited)
    gl_probe_gateway_chain_inited_ready
    ;;
  gl.gateway_settlement)
    gl_probe_gateway_settlement_ready
    ;;
  gl.os_configs_gateway)
    gl_probe_os_configs_gateway_ready
    ;;
  gl.edge_chain_inited)
    gl_probe_edge_chain_inited_ready
    ;;
  gl.migration)
    "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" >/dev/null 2>&1
    ;;
  gl.os_configs_final)
    gl_probe_os_configs_final_ready
    ;;
  *)
    return 1
    ;;
  esac
}

perform_repair_step() {
  local checkpoint_id="${1:?checkpoint id required}"
  case "${checkpoint_id}" in
  gl.workspace)
    gl_l1_broadcast_preflight
    ;;
  gl.ecosystem)
    if [ ! -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
      "${SCRIPT_DIR}/gateway-ecosystem-create.sh"
    fi
    gl_resolve_gateway_dir_after_ecosystem_create
    ;;
  gl.wallets_funded)
    "${SCRIPT_DIR}/fund-wallets.sh"
    ;;
  gl.l1_ecosystem_deployed)
    gl_clear_os_server_chain_db "${GATEWAY_CHAIN_NAME:-gateway}"
    "${SCRIPT_DIR}/gateway-deploy-l1.sh"
    ;;
  gl.gateway_chain_inited)
    "${SCRIPT_DIR}/gateway-chain-init.sh"
    ;;
  gl.gateway_settlement)
    "${SCRIPT_DIR}/gateway-convert-settlement.sh"
    ;;
  gl.os_configs_gateway)
    env MATERIALIZE_EDGE_CONFIG=false "${SCRIPT_DIR}/generate-os-server-configs.sh"
    ;;
  gl.edge_chain_inited)
    gl_clear_os_server_chain_db "${EDGE_CHAIN_NAME:-zksys}"
    "${SCRIPT_DIR}/edge-chain-create-init.sh"
    ;;
  gl.migration)
    "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh"
    ;;
  gl.os_configs_final)
    "${SCRIPT_DIR}/generate-os-server-configs.sh"
    ;;
  *)
    gl_die "unknown checkpoint: ${checkpoint_id}"
    ;;
  esac
}

if [ "${COMMAND}" = "status" ]; then
  state_file="$(gl_checkpoint_state_file)"
  echo "state_file: ${state_file}"
  python3 - "${state_file}" <<'PY'
import json
import sys
from pathlib import Path

state_path = Path(sys.argv[1])
if not state_path.exists():
    print("state: not initialized")
    raise SystemExit(0)
state = json.loads(state_path.read_text(encoding="utf-8"))
print("run_id:", state.get("run_id"))
print("updated_at:", state.get("updated_at"))
print("current_checkpoint:", state.get("current_checkpoint"))
print("last_error:", state.get("last_error"))
print("checkpoints:")
for key, value in sorted((state.get("checkpoints") or {}).items()):
    print(f"  - {key}: {value.get('status')} ({value.get('at')})")
PY
  exit 0
fi

if [ "${COMMAND}" != "repair" ]; then
  usage 1
fi

checkpoint_is_known "${CHECKPOINT_ID}" || gl_die "unknown checkpoint id: ${CHECKPOINT_ID}"

if validate_checkpoint "${CHECKPOINT_ID}"; then
  gl_checkpoint_mark_repaired "${CHECKPOINT_ID}" "already valid; no repair command needed"
  echo "gateway-launch-repair: ${CHECKPOINT_ID} already valid; marked repaired"
  exit 0
fi

echo "gateway-launch-repair: repairing ${CHECKPOINT_ID}"
gl_checkpoint_mark_in_progress "${CHECKPOINT_ID}"

set +e
perform_repair_step "${CHECKPOINT_ID}"
step_rc=$?
set -e

if [ "${step_rc}" -ne 0 ]; then
  gl_checkpoint_mark_blocked "${CHECKPOINT_ID}" "repair command failed with exit code ${step_rc}"
  exit "${step_rc}"
fi

if ! validate_checkpoint "${CHECKPOINT_ID}"; then
  gl_checkpoint_mark_blocked "${CHECKPOINT_ID}" "repair command completed but validation failed"
  gl_die "checkpoint validation failed after repair: ${CHECKPOINT_ID}"
fi

gl_checkpoint_mark_repaired "${CHECKPOINT_ID}" "repaired via gateway-launch-repair.sh"
echo "gateway-launch-repair: ${CHECKPOINT_ID} repaired and validated"
