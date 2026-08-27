#!/usr/bin/env bash
# Canonical launcher for Gateway + Edge on Tanenbaum/Mainnet.
# This script runs a fixed checkpointed pipeline. No user-facing skip/with/anvil controls.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORIG_ARGS=("$@")
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_gateway_node_lifecycle.sh"
gl_validate_prover_mode

# SYSCOIN: do not source HOME-relative Cargo env files in the deployment
# launcher. _common.sh already prepends ~/.cargo/bin to PATH without executing
# attacker-controlled shell code in the process that carries deployment secrets.

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
  GATEWAY_ARCHIVE_L1_RPC_URL   archive L1 RPC for startup history and settlement proofs (defaults to L1_RPC_URL)
  BITCOIN_DA_MIN_BALANCE_SYS    target DA wallet balance, default 10 on Tanenbaum, 0 on mainnet
  PROTOCOL_VERSION             default v32.0
  GATEWAY_DIR                  default ~/gateway
  REUSE_ECOSYSTEM              true|false, default false
  MIGRATE_EDGE                 true|false, default false
  PROVER_MODE                  gpu|no-proofs (default gpu)
  GATEWAY_PROVER_MODE          ecosystem prover mode, defaults from PROVER_MODE
  GATEWAY_LAUNCH_LOG           default ~/gateway-launch.log
  GATEWAY_INTEROP_FEE_USD      Gateway interop fee target per call, default 0.15
  NATIVE_TOKEN_PRICE_USD       native SYS price used for forced prices and fee target, default 0.01
  GATEWAY_SETTLEMENT_FEE       optional explicit fee in base units; overrides USD calculation
  L1_WETH_TOKEN_ADDRESS        optional L1 wrapped native token override; defaults to WSYS on Syscoin L1
  BITCOIN_DA_RPC_URL / BITCOIN_DA_RPC_USER / BITCOIN_DA_RPC_PASSWORD
  GATEWAY_FUND_WALLETS_PATHS   optional extra wallets.yaml list (colon-separated)
  FUNDER_SIGNER                account|keystore|ledger|trezor|aws|gcp (default account)
  DEPLOYER_SIGNER              optional override; defaults to FUNDER_SIGNER
  EDGE_GATEWAY_GOVERNOR_SIGNER optional override; defaults to FUNDER_SIGNER

Options:
  --l1 tanenbaum|mainnet
  --log PATH
  --reuse-ecosystem            reuse an existing GATEWAY_DIR/ZkStack.yaml instead of creating wallets/ecosystem
  --migrate-edge               pause deposits and migrate/finalize edge settlement to Gateway
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
  --reuse-ecosystem)
    REUSE_ECOSYSTEM=true
    shift
    ;;
  --migrate-edge)
    MIGRATE_EDGE=true
    shift
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
  # SYSCOIN: preserve child failures when the launcher allocates a Linux PTY.
  exec script -e -q -c "$(printf '%q ' "${_q[@]}")" /dev/null
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
  : "${BITCOIN_DA_MIN_BALANCE_SYS:=10}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL BITCOIN_DA_MIN_BALANCE_SYS ETH_GAS_PRICE ETH_PRIORITY_GAS_PRICE
  ;;
mainnet)
  export L1_CHAIN_ID=57
  export L1_NETWORK=mainnet
  gl_require L1_RPC_URL
  : "${BITCOIN_DA_RPC_URL:=http://127.0.0.1:8370}"
  : "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
  : "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
  : "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
  : "${BITCOIN_DA_MIN_BALANCE_SYS:=0}"
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD BITCOIN_DA_FINALITY_MODE BITCOIN_DA_FINALITY_CONFIRMATIONS BITCOIN_DA_PODA_URL BITCOIN_DA_MIN_BALANCE_SYS ETH_GAS_PRICE ETH_PRIORITY_GAS_PRICE
  ;;
*)
  gl_die "invalid --l1: ${L1_PROFILE} (supported: tanenbaum|mainnet)"
  ;;
esac
gl_reject_no_proofs_on_mainnet
gl_validate_l1_signer_policy
gl_normalize_canonical_deployment_inputs

case "${L1_RPC_URL}" in
http://* | https://*) ;;
*)
  gl_die "L1_RPC_URL must be http:// or https://"
  ;;
esac

gl_export_foundry_evm_version
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
# Keep zkstack's internal forge invocations deterministic and prevent network
# lookups from stalling deployment after local compilation/simulation.
export FOUNDRY_OFFLINE="${FOUNDRY_OFFLINE:-true}"
export GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
export GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME:-gateway}"
export EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
gl_resolve_gateway_dir planned
gl_acquire_gateway_launch_lock
# SYSCOIN: The checkpointed launcher targets the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
: "${REUSE_ECOSYSTEM:=false}"
REUSE_ECOSYSTEM="$(gl_to_lower "${REUSE_ECOSYSTEM}")"
case "${REUSE_ECOSYSTEM}" in
true | false) ;;
*) gl_die "invalid REUSE_ECOSYSTEM='${REUSE_ECOSYSTEM}' (expected: true | false)" ;;
esac
export REUSE_ECOSYSTEM
: "${MIGRATE_EDGE:=false}"
MIGRATE_EDGE="$(gl_to_lower "${MIGRATE_EDGE}")"
case "${MIGRATE_EDGE}" in
true | false) ;;
*) gl_die "invalid MIGRATE_EDGE='${MIGRATE_EDGE}' (expected: true | false)" ;;
esac
export MIGRATE_EDGE
if [ "${REUSE_ECOSYSTEM}" = true ] &&
  { [ -n "${GATEWAY_WALLET_CREATION:-}" ] || [ -n "${GATEWAY_WALLET_PATH:-}" ]; }; then
  gl_die "GATEWAY_WALLET_CREATION/GATEWAY_WALLET_PATH are ignored with --reuse-ecosystem; unset them or use a fresh GATEWAY_DIR"
fi
# SYSCOIN: Resolve pins unconditionally so pre-set REQUIRED_* values cannot
# bypass the pending-fixture policy through lazy shell expansion.
gl_resolve_required_source_pins


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


run_migrate_edge_with_retry() {
  local attempt max_attempts status migrate_output
  local migrate_output_lc
  max_attempts="$(normalize_migration_start_uint \
    GATEWAY_MIGRATE_EDGE_MAX_ATTEMPTS \
    "${GATEWAY_MIGRATE_EDGE_MAX_ATTEMPTS:-2}" 10)" || return $?
  [ "${max_attempts}" -gt 0 ] || max_attempts=1
  for attempt in $(seq 1 "${max_attempts}"); do
    if migrate_output="$("${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" 2>&1)"; then
      status=0
    else
      status=$?
    fi
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
      "${SCRIPT_DIR}/fund-wallets.sh" || return $?
      continue
    fi
    return "${status}"
  done
}

cleanup() {
  stop_gateway_for_migration || true
}
handle_interrupt() {
  cleanup
  trap - EXIT INT TERM
  exit 130
}
handle_terminate() {
  cleanup
  trap - EXIT INT TERM
  exit 143
}
trap cleanup EXIT
trap handle_interrupt INT
trap handle_terminate TERM

run_checkpoint_with_validation() {
  local checkpoint_id="${1:?checkpoint id required}"
  local validator_fn="${2:?validator function required}"
  local status
  shift 2

  status="$(gl_checkpoint_get_status "${checkpoint_id}")" || return $?
  case "${status}" in
  passed)
    if ("${validator_fn}"); then
      echo "checkpoint ${checkpoint_id} already passed and revalidated; skipping"
      return 0
    fi
    gl_checkpoint_mark_blocked "${checkpoint_id}" "passed checkpoint failed live validation; explicit repair required" || return $?
    gl_die "checkpoint ${checkpoint_id} no longer satisfies its postcondition; run gateway-launch-repair.sh repair ${checkpoint_id}"
    ;;
  pending) ;;
  blocked)
    gl_die "checkpoint ${checkpoint_id} is blocked; run gateway-launch-repair.sh repair ${checkpoint_id}"
    ;;
  *)
    gl_checkpoint_mark_blocked "${checkpoint_id}" "unsafe prior status ${status}; explicit repair required" || return $?
    gl_die "checkpoint ${checkpoint_id} was ${status}; run gateway-launch-repair.sh repair ${checkpoint_id} instead of replaying it automatically"
    ;;
  esac

  gl_checkpoint_run "${checkpoint_id}" "$@" || return $?
  if ! ("${validator_fn}"); then
    gl_checkpoint_mark_blocked "${checkpoint_id}" "post-run validation failed" || return $?
    gl_die "checkpoint ${checkpoint_id} validation failed after command success"
  fi
}

validate_workspace() { gl_probe_workspace_ready; }
validate_ecosystem() {
  gl_probe_ecosystem_ready && gl_assert_gateway_chain_config_matches_expected
}
validate_wallets_funded() { gl_probe_wallets_funded_ready; }
validate_l1_deployed() { gl_probe_l1_ecosystem_deployed_ready; }
validate_gateway_chain_inited() { gl_probe_gateway_chain_inited_ready; }
validate_gateway_settlement() { gl_probe_gateway_settlement_ready; }
validate_os_configs_gateway() { gl_probe_os_configs_gateway_ready; }
validate_edge_chain_inited() { gl_probe_edge_chain_inited_and_governor_ready; }
validate_migration() {
  "${SCRIPT_DIR}/edge-chain-migrate-to-gateway.sh" --check-only
}
validate_os_configs_final() { gl_probe_os_configs_final_ready; }

step_workspace() {
  wait_for_rpc
  gl_l1_broadcast_preflight
}

step_ecosystem() {
  if [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
    if [ "${REUSE_ECOSYSTEM}" != true ]; then
      # SYSCOIN: do not silently bypass gateway-ecosystem-create.sh; that is where
      # wallet creation/path controls are applied before funding and deployment.
      gl_die "existing ecosystem found at ${GATEWAY_DIR}; pass --reuse-ecosystem to trust and reuse it, or choose/remove GATEWAY_DIR"
    fi
    echo "gateway-launch: reusing existing ecosystem at ${GATEWAY_DIR}"
  else
    if [ "${REUSE_ECOSYSTEM}" = true ]; then
      gl_die "--reuse-ecosystem requested but no ecosystem exists at ${GATEWAY_DIR}/ZkStack.yaml"
    fi
    "${SCRIPT_DIR}/gateway-ecosystem-create.sh" || return $?
  fi
  gl_resolve_gateway_dir
}

step_l1_ecosystem_deployed() {
  # SYSCOIN: Never mutate runtime DB state as a side effect of an L1 repair or
  # retry. A genuinely fresh deployment has no DB; incompatible state requires
  # an explicit stopped-node backup/reset by the operator.
  env GATEWAY_ECOSYSTEM_RESUME_FIRST=false \
    "${SCRIPT_DIR}/gateway-deploy-l1.sh"
}

step_edge_chain_inited() {
  "${SCRIPT_DIR}/edge-chain-create-init.sh"
}

echo "gateway-launch: initializing checkpoint state"
gl_checkpoint_state_init
wait_for_rpc
gl_ensure_zksync_era_workspace
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
gl_checkpoint_set_fingerprint_if_empty
gl_checkpoint_assert_fingerprint_matches

run_checkpoint_with_validation "gl.workspace" validate_workspace step_workspace || exit $?
run_checkpoint_with_validation "gl.ecosystem" validate_ecosystem step_ecosystem || exit $?
run_checkpoint_with_validation "gl.wallets_funded" validate_wallets_funded "${SCRIPT_DIR}/fund-wallets.sh" || exit $?
run_checkpoint_with_validation "gl.l1_ecosystem_deployed" validate_l1_deployed step_l1_ecosystem_deployed || exit $?
run_checkpoint_with_validation "gl.gateway_chain_inited" validate_gateway_chain_inited "${SCRIPT_DIR}/gateway-chain-init.sh" || exit $?
run_checkpoint_with_validation "gl.gateway_settlement" validate_gateway_settlement "${SCRIPT_DIR}/gateway-convert-settlement.sh" || exit $?
# SYSCOIN: The pending source-only mock route may deploy and convert a fresh
# Gateway, but a different deployment identity requires an app repin. Stop
# before creating an edge or compiling a node against incompatible constants.
gl_assert_gateway_config_identity
run_checkpoint_with_validation "gl.os_configs_gateway" validate_os_configs_gateway env MATERIALIZE_EDGE_CONFIG=false "${SCRIPT_DIR}/generate-os-server-configs.sh" || exit $?
# SYSCOIN: Authenticate the live Gateway postimages before an edge can be
# created against it. Keep this node running through migration when requested.
start_gateway_for_migration || exit $?
run_checkpoint_with_validation "gl.edge_chain_inited" validate_edge_chain_inited step_edge_chain_inited || exit $?

migration_status="$(gl_checkpoint_get_status "gl.migration")" || exit $?
case "${migration_status}" in
pending | passed) ;;
blocked)
  gl_die "checkpoint gl.migration is blocked; run gateway-launch-repair.sh repair gl.migration"
  ;;
*)
  gl_checkpoint_mark_blocked "gl.migration" "unsafe prior status ${migration_status}; explicit repair required" || exit $?
  gl_die "checkpoint gl.migration was ${migration_status}; run gateway-launch-repair.sh repair gl.migration"
  ;;
esac

if [ "${MIGRATE_EDGE}" != true ] && [ "${migration_status}" = "pending" ]; then
  # SYSCOIN: edge migration pauses deposits and finalizes settlement changes, so
  # require an explicit operator opt-in before running it with deployment keys.
  echo "gateway-launch: edge chain is initialized; migration was not run."
  echo "gateway-launch: rerun this command with --migrate-edge when ready to pause deposits and migrate/finalize edge settlement."
  echo "gateway-launch: final os-server configs will be generated after migration completes."
  stop_gateway_for_migration
  trap - EXIT INT TERM
  exit 0
fi

if [ "${migration_status}" = "passed" ]; then
  if (validate_migration); then
    echo "checkpoint gl.migration already passed and revalidated; skipping"
  else
    migration_rc=$?
    stop_gateway_for_migration || true
    gl_checkpoint_mark_blocked "gl.migration" "passed migration failed read-only validation with exit code ${migration_rc}" || exit $?
    gl_die "checkpoint gl.migration no longer satisfies its postconditions; run gateway-launch-repair.sh repair gl.migration"
  fi
else
  gl_checkpoint_mark_in_progress "gl.migration" || exit $?
  migration_rc=0
  if start_gateway_for_migration; then
    if run_migrate_edge_with_retry; then
      if (validate_migration); then
        :
      else
        migration_rc=$?
      fi
    else
      migration_rc=$?
    fi
  else
    migration_rc=$?
  fi
  if stop_gateway_for_migration; then
    :
  else
    stop_rc=$?
    if [ "${migration_rc}" -eq 0 ]; then
      migration_rc="${stop_rc}"
    fi
  fi
  if [ "${migration_rc}" -ne 0 ]; then
    gl_checkpoint_mark_blocked "gl.migration" "migration failed with exit code ${migration_rc}" || exit $?
    exit "${migration_rc}"
  fi
  gl_checkpoint_mark_passed "gl.migration" || exit $?
fi

# The identity-attestation start above may also run when migration was already
# checkpointed. Never leave that temporary process behind.
stop_gateway_for_migration

run_checkpoint_with_validation "gl.os_configs_final" validate_os_configs_final "${SCRIPT_DIR}/generate-os-server-configs.sh" || exit $?

echo "=== gateway-launch complete ==="
trap - EXIT INT TERM
