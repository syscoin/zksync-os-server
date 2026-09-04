#!/usr/bin/env bash
# Explicit checkpoint repair helper for run-gateway-launch.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_gateway_node_lifecycle.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_execute_operator_lock.sh"
L1_PROFILE=""
COMMAND=""
CHECKPOINT_ID=""
MIGRATE_EDGE_REQUESTED=false

usage() {
  cat <<'EOF'
gateway-launch-repair.sh --l1 tanenbaum|mainnet [--migrate-edge] status
gateway-launch-repair.sh --l1 tanenbaum|mainnet [--migrate-edge] repair <checkpoint-id>

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
  --migrate-edge)
    MIGRATE_EDGE_REQUESTED=true
    shift
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

if [ "${MIGRATE_EDGE_REQUESTED}" = true ]; then
  MIGRATE_EDGE=true
else
  : "${MIGRATE_EDGE:=false}"
fi
MIGRATE_EDGE="$(gl_to_lower "${MIGRATE_EDGE}")"
case "${MIGRATE_EDGE}" in true | false) ;; *) gl_die "MIGRATE_EDGE must be true or false" ;; esac
export MIGRATE_EDGE

[ -n "${L1_PROFILE}" ] || gl_die "required: --l1 tanenbaum|mainnet"

case "${L1_PROFILE}" in
tanenbaum)
  export L1_CHAIN_ID=5700
  export L1_NETWORK=tanenbaum
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:18370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Confirmations}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.tanenbaum.io}"
  : "${BITCOIN_DA_MIN_BALANCE_SYS:=10}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL BITCOIN_DA_MIN_BALANCE_SYS
  ;;
mainnet)
  export L1_CHAIN_ID=57
  export L1_NETWORK=mainnet
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:8370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
  : "${BITCOIN_DA_MIN_BALANCE_SYS:=0}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL BITCOIN_DA_MIN_BALANCE_SYS
  ;;
*)
  gl_die "invalid --l1: ${L1_PROFILE}"
  ;;
esac

export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
export GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME:-gateway}"
export EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
gl_resolve_gateway_dir planned
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

# SYSCOIN: Repair owns the single canonical checkpoint graph. Additional edges
# have independent live state and must never validate or rewrite this journal.
gl_is_canonical_edge_context ||
  gl_die "gateway-launch-repair.sh supports only canonical zksys chain 57057"
gl_validate_prover_mode
install_gateway_migration_cleanup_traps
if [ -z "${GATEWAY_PROVER_MODE:-}" ]; then
  if [ "${PROVER_MODE}" = "no-proofs" ]; then
    export GATEWAY_PROVER_MODE="no-proofs"
  else
    export GATEWAY_PROVER_MODE="gpu"
  fi
fi

gl_require L1_RPC_URL
gl_reject_no_proofs_on_mainnet
gl_validate_l1_signer_policy
gl_normalize_canonical_deployment_inputs

case "${L1_RPC_URL}" in
http://* | https://*) ;;
*) gl_die "L1_RPC_URL must be http:// or https://" ;;
esac

gl_export_foundry_evm_version
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
gl_l1_broadcast_preflight
# SYSCOIN: Keep explicit repairs on the launcher's deterministic Forge path.
export FOUNDRY_OFFLINE="${FOUNDRY_OFFLINE:-true}"
gl_acquire_gateway_launch_lock
# SYSCOIN: Repair checkpoints only for the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins

gl_ensure_zksync_era_workspace
gl_ensure_zkstack_cli_release_current
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
    gl_probe_ecosystem_ready && gl_assert_gateway_chain_config_matches_expected
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
    gl_probe_edge_chain_inited_and_governor_ready
    ;;
  gl.migration)
    "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" --check-only >/dev/null 2>&1
    ;;
  gl.os_configs_final)
    gl_probe_os_configs_final_ready
    ;;
  *)
    return 1
    ;;
  esac
}

handle_direct_gateway_validation_exit() {
  local exit_rc=$?
  trap '' INT TERM
  trap - EXIT
  # SYSCOIN: retire a fatally aborted supervised repair only after its exact
  # command group and launcher-owned Gateway node have been cleaned up.
  cleanup_gateway_for_migration_on_exit
  if [ "${exit_rc}" -ne 0 ]; then
    (
      gl_checkpoint_mark_blocked \
        "${CHECKPOINT_ID}" "supervised repair aborted with exit code ${exit_rc}"
    ) || true
  fi
  exit "${exit_rc}"
}

run_supervised_gateway_repair_operation() {
  local operation_rc=0
  # SYSCOIN: mutating recovery and read-only validation share one scoped,
  # journal-aware owned-group lifecycle.
  trap handle_direct_gateway_validation_exit EXIT
  GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND=true
  "$@" || operation_rc=$?
  GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND=false
  install_gateway_migration_cleanup_traps
  return "${operation_rc}"
}

validate_checkpoint_for_repair() {
  case "${1:?checkpoint id required}" in
  gl.edge_chain_inited)
    # SYSCOIN: Edge-init readiness is local/L1-only. Do not start the Gateway
    # node for a validator that must remain incapable of broadcasting.
    run_supervised_gateway_repair_operation \
      run_gateway_repair_validator_in_owned_group validate_checkpoint "$1"
    ;;
  gl.migration)
    # SYSCOIN: keep Gateway state in this repair shell, but isolate the actual
    # validator and all of its descendants in an exact supervised process group.
    run_supervised_gateway_repair_operation \
      run_with_gateway_for_migration validate_checkpoint "$1"
    ;;
  *) (validate_checkpoint "$1") ;;
  esac
}

# SYSCOIN: gl_die exits its shell. Keep classifier failures as ordinary owned-
# group return codes so repair can safely test the one alternate exact state.
classify_edge_post_admin_resume_safe() {
  (gl_assert_edge_post_admin_resume_safe)
}

classify_edge_created_only_resume_safe() {
  (gl_assert_edge_created_only_resume_safe)
}

perform_repair_step() {
  local checkpoint_id="${1:?checkpoint id required}"
  case "${checkpoint_id}" in
  gl.workspace)
    gl_l1_broadcast_preflight
    ;;
  gl.ecosystem)
    if [ ! -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
      "${SCRIPT_DIR}/gateway-ecosystem-create.sh" || return $?
    fi
    gl_resolve_gateway_dir
    ;;
  gl.wallets_funded)
    "${SCRIPT_DIR}/fund-wallets.sh"
    ;;
  gl.l1_ecosystem_deployed)
    case "${REPAIR_PRIOR_STATUS}" in
    blocked | in_progress | passed)
      env GATEWAY_ECOSYSTEM_RESUME_FIRST=true \
        "${SCRIPT_DIR}/gateway-deploy-l1.sh"
      ;;
    pending)
      env GATEWAY_ECOSYSTEM_RESUME_FIRST=false \
        "${SCRIPT_DIR}/gateway-deploy-l1.sh"
      ;;
    *) gl_die "unsupported prior checkpoint status for L1 repair: ${REPAIR_PRIOR_STATUS}" ;;
    esac
    ;;
  gl.gateway_chain_inited)
    echo "gateway-launch-repair: chain init is multi-stage and is not safe to replay automatically" >&2
    return 1
  ;;
  gl.gateway_settlement)
    echo "gateway-launch-repair: Gateway conversion is multi-stage and is not safe to replay automatically" >&2
    return 1
    ;;
  gl.os_configs_gateway)
    env MATERIALIZE_EDGE_CONFIG=false "${SCRIPT_DIR}/generate-os-server-configs.sh"
    ;;
  gl.edge_chain_inited)
    # SYSCOIN: chain init is not replay-safe. Keep either exact remainder and
    # all descendants in repair's bounded owned PGID.
    case "${REPAIR_PRIOR_STATUS}" in
    in_progress | blocked)
      # SYSCOIN: Classify and run every descendant in an exact owned PGID.
      # A failed created-only retry may legitimately leave this checkpoint blocked.
      if run_supervised_gateway_repair_operation \
        run_gateway_repair_validator_in_owned_group \
        classify_edge_post_admin_resume_safe >/dev/null 2>&1; then
        run_supervised_gateway_repair_operation \
          run_gateway_repair_validator_in_owned_group \
          env GATEWAY_EDGE_POST_ADMIN_REPAIR=true \
            "${SCRIPT_DIR}/edge-chain-create-init.sh" --resume-post-admin
      elif run_supervised_gateway_repair_operation \
        run_gateway_repair_validator_in_owned_group \
        classify_edge_created_only_resume_safe >/dev/null 2>&1; then
        run_supervised_gateway_repair_operation \
          run_with_gateway_for_migration env \
            GATEWAY_EDGE_CREATED_ONLY_REPAIR=true \
            "${SCRIPT_DIR}/edge-chain-create-init.sh" --resume-created-only
      else
        echo "gateway-launch-repair: edge init state is neither exact created-only nor exact post-admin" >&2
        return 1
      fi
      ;;
    *)
      echo "gateway-launch-repair: edge init can resume only an exact created-only or post-admin state" >&2
      return 1
      ;;
    esac
  ;;
  gl.migration)
    # SYSCOIN: never replay pause/migrate/finalize. The child first proves the
    # complete L1 finalization postcondition, then resumes only idempotent
    # post-migration reconciliation and its exact private Forge journals. Bind
    # it to this checkpoint owner with the same inode-validated nonce-lock
    # capability used by a normal migration launch.
    case "${REPAIR_PRIOR_STATUS}" in
    blocked | in_progress | passed)
      gateway_acquire_execute_operator_lock "${EDGE_CHAIN_NAME}" || return $?
      export GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD="${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}"
      run_supervised_gateway_repair_operation \
        run_with_gateway_for_migration \
        "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" --resume-post-finalize
      ;;
    *)
      echo "gateway-launch-repair: gl.migration post-finalize repair requires a blocked, in-progress, or previously passed checkpoint" >&2
      return 1
      ;;
    esac
    ;;
  gl.os_configs_final)
    "${SCRIPT_DIR}/generate-os-server-configs.sh"
    ;;
  *)
    gl_die "unknown checkpoint: ${checkpoint_id}"
    ;;
  esac
}

if [ "${COMMAND}" != "repair" ]; then
  usage 1
fi

checkpoint_is_known "${CHECKPOINT_ID}" || gl_die "unknown checkpoint id: ${CHECKPOINT_ID}"
REPAIR_PRIOR_STATUS="$(gl_checkpoint_get_status "${CHECKPOINT_ID}")"

# SYSCOIN: Retiring a late-success sender journal must be atomic with the live
# reserve check relative to supported edge-node spending. Keep this lock
# through initial validation, post-finalize reconciliation, and revalidation.
case "${CHECKPOINT_ID}:${REPAIR_PRIOR_STATUS}" in
gl.migration:blocked | gl.migration:in_progress | gl.migration:passed)
  gateway_acquire_execute_operator_lock "${EDGE_CHAIN_NAME}"
  export GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD="${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}"
  ;;
esac

if validate_checkpoint_for_repair "${CHECKPOINT_ID}"; then
  case "${CHECKPOINT_ID}:${REPAIR_PRIOR_STATUS}" in
  gl.migration:blocked | gl.migration:in_progress | gl.migration:passed)
    # SYSCOIN: A via-Gateway admin transaction may satisfy its postcondition
    # after a wait timed out. Reuse the locked, post-finalize-only path below
    # so it retires the completed Forge journal before this checkpoint passes.
    echo "gateway-launch-repair: ${CHECKPOINT_ID} already valid; reconciling completed repair journals"
    ;;
  *)
    gl_checkpoint_mark_repaired "${CHECKPOINT_ID}" "already valid; no repair command needed"
    echo "gateway-launch-repair: ${CHECKPOINT_ID} already valid; marked repaired"
    exit 0
    ;;
  esac
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

if ! validate_checkpoint_for_repair "${CHECKPOINT_ID}"; then
  gl_checkpoint_mark_blocked "${CHECKPOINT_ID}" "repair command completed but validation failed"
  gl_die "checkpoint validation failed after repair: ${CHECKPOINT_ID}"
fi

gl_checkpoint_mark_repaired "${CHECKPOINT_ID}" "repaired via gateway-launch-repair.sh"
echo "gateway-launch-repair: ${CHECKPOINT_ID} repaired and validated"
