# shellcheck shell=bash
# Shared, launcher-owned Gateway node lifecycle for migration and repair.
# Source only after _common.sh.
[ -n "${GL_DIR:-}" ] || {
  echo "gateway-launch: _gateway_node_lifecycle.sh requires _common.sh" >&2
  return 1
}

json_rpc_hex_to_dec() {
  local rpc_url="${1:?rpc url required}"
  local method="${2:?rpc method required}"
  local request_deadline_ms="${3:-0}"
  python3 - "${rpc_url}" "${method}" "${request_deadline_ms}" <<'PY'
import json
import re
import signal
import sys
import time
import urllib.request

rpc_url = sys.argv[1]
method = sys.argv[2]
deadline_raw = sys.argv[3]
if not deadline_raw.isdecimal():
    raise SystemExit(1)
deadline_ms = int(deadline_raw, 10)
if deadline_ms:
    remaining_s = (deadline_ms - time.monotonic_ns() // 1_000_000) / 1000
    if remaining_s <= 0:
        raise SystemExit(1)
    timeout_s = min(remaining_s, 3.0)
else:
    timeout_s = 3.0

# SYSCOIN: urllib's socket timeout is per operation. Bound the complete local
# readiness probe so a partial response cannot overrun the startup deadline.
signal.signal(signal.SIGALRM, lambda *_: sys.exit(1))
signal.setitimer(signal.ITIMER_REAL, timeout_s)
payload = json.dumps(
    {"jsonrpc": "2.0", "method": method, "params": [], "id": 1}
).encode("utf-8")
req = urllib.request.Request(
    rpc_url,
    data=payload,
    headers={
        "Content-Type": "application/json",
        # SYSCOIN: Tanenbaum RPC rejects Python's default urllib user agent.
        "User-Agent": "gateway-launch/1.0",
    },
    method="POST",
)
with urllib.request.urlopen(req, timeout=timeout_s) as resp:
    body = resp.read().decode("utf-8")
obj = json.loads(body)
result = obj.get("result")
if not isinstance(result, str) or len(result) > 66 or not re.fullmatch(
    r"0x(?:0|[1-9a-fA-F][0-9a-fA-F]*)", result
):
    raise SystemExit(1)
output = str(int(result, 16))
signal.setitimer(signal.ITIMER_REAL, 0)
print(output)
PY
}
gateway_rpc_ready() {
  local gateway_rpc="${1:-$(gl_gateway_runtime_rpc_url)}"
  local request_deadline_ms="${2:-0}" block_no
  block_no="$(json_rpc_hex_to_dec "${gateway_rpc}" "eth_blockNumber" "${request_deadline_ms}" 2>/dev/null || true)"
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
import sys
from pathlib import Path

config_path = Path(sys.argv[1])
new_rpc_url = sys.argv[2]
lines = config_path.read_text(encoding="utf-8").splitlines(keepends=True)
in_l1_provider = False
in_l1_archive_provider = False
patched = False

for idx, line in enumerate(lines):
    stripped = line.strip()
    if line and not line.startswith((" ", "\t")):
        in_l1_provider = stripped == "l1_provider:"
        in_l1_archive_provider = stripped == "l1_archive_provider:"
        continue
    if in_l1_archive_provider and stripped.startswith("rpc_url:"):
        indent = line[: len(line) - len(line.lstrip())]
        newline = "\n" if line.endswith("\n") else ""
        lines[idx] = f"{indent}rpc_url: {json.dumps(new_rpc_url)}{newline}"
        patched = True
        break

if not patched:
    insert_at = None
    for idx, line in enumerate(lines):
        if line.strip() == "l1_provider:":
            insert_at = idx + 1
            while insert_at < len(lines) and (
                not lines[insert_at].strip() or lines[insert_at].startswith((" ", "\t"))
            ):
                insert_at += 1
            break
    if insert_at is None:
        raise SystemExit(f"failed to find l1_provider section in {config_path}")
    lines[insert_at:insert_at] = [
        "l1_archive_provider:\n",
        f"  rpc_url: {json.dumps(new_rpc_url)}\n",
    ]
    patched = True

config_path.write_text("".join(lines), encoding="utf-8")
print(f"gateway-launch: set {config_path} l1_archive_provider.rpc_url -> {new_rpc_url}")
PY
}

GATEWAY_NODE_PID=""
GATEWAY_STARTED_FOR_MIGRATION=false
GATEWAY_MIGRATION_FOREGROUND_PID=""
GATEWAY_MIGRATION_VALIDATOR_PID=""
GATEWAY_MIGRATION_VALIDATOR_CONTROL_DIR=""
GATEWAY_MIGRATION_CANCEL_SIGNAL=""
GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND=false
unset GATEWAY_RUNTIME_OWNER_PID

normalize_migration_start_uint() {
  local name="${1:?name required}"
  local raw="${2:?value required}"
  local max="${3:?max required}"
  python3 - "${name}" "${raw}" "${max}" <<'PY'
import sys

name, raw, max_raw = sys.argv[1:]
if not raw.isdecimal():
    raise SystemExit(f"{name} must be an unsigned decimal integer")
value = int(raw, 10)
max_value = int(max_raw, 10)
if value > max_value:
    raise SystemExit(f"{name} must be <= {max_value}")
print(value)
PY
}

# SYSCOIN: macOS ships Bash 3.2 without a timed wait. Use monotonic milliseconds
# and the shell's exact background-job table for portable bounded waits.
migration_monotonic_millis() {
  python3 - <<'PY'
import time
print(time.monotonic_ns() // 1_000_000)
PY
}

migration_millis_as_seconds() {
  local millis="${1:?milliseconds required}"
  printf '%d.%03d\n' "$((millis / 1000))" "$((millis % 1000))"
}

gateway_launcher_job_is_active() {
  local expected_pid="${1:?expected PID required}" job_pid
  while IFS= read -r job_pid; do
    [ "${job_pid}" = "${expected_pid}" ] && return 0
  done < <(jobs -p)
  return 1
}

gateway_wait_for_rpc_start() {
  local gateway_pid="${1:?Gateway PID required}"
  local gateway_rpc="${2:?Gateway RPC required}"
  local timeout_s="${3:?startup timeout required}"
  local poll_s="${4:?startup poll required}"
  local deadline now remaining_ms sleep_ms

  now="$(migration_monotonic_millis)" || return $?
  deadline=$((now + timeout_s * 1000))
  while gateway_launcher_job_is_active "${gateway_pid}"; do
    now="$(migration_monotonic_millis)" || return $?
    remaining_ms=$((deadline - now))
    [ "${remaining_ms}" -gt 0 ] || return 124
    gateway_rpc_ready "${gateway_rpc}" "${deadline}" && return 0
    gateway_launcher_job_is_active "${gateway_pid}" || return 125
    now="$(migration_monotonic_millis)" || return $?
    remaining_ms=$((deadline - now))
    [ "${remaining_ms}" -gt 0 ] || return 124
    sleep_ms=$((poll_s * 1000))
    [ "${sleep_ms}" -le "${remaining_ms}" ] || sleep_ms="${remaining_ms}"
    migration_interruptible_sleep "$(migration_millis_as_seconds "${sleep_ms}")" || return $?
  done
  return 125
}

gateway_wait_for_job_exit() {
  local gateway_pid="${1:?Gateway PID required}"
  local timeout_s="${2:?shutdown timeout required}"
  local deadline now remaining_ms sleep_ms

  now="$(migration_monotonic_millis)" || return $?
  deadline=$((now + timeout_s * 1000))
  while gateway_launcher_job_is_active "${gateway_pid}"; do
    now="$(migration_monotonic_millis)" || return $?
    remaining_ms=$((deadline - now))
    [ "${remaining_ms}" -gt 0 ] || return 1
    sleep_ms=200
    [ "${sleep_ms}" -le "${remaining_ms}" ] || sleep_ms="${remaining_ms}"
    sleep "$(migration_millis_as_seconds "${sleep_ms}")"
  done
}

gateway_job_leads_process_group() {
  local group_leader_pid="${1:?group leader PID required}" actual_pgid
  gateway_launcher_job_is_active "${group_leader_pid}" || return 1
  actual_pgid="$(python3 - "${group_leader_pid}" <<'PY'
import os
import sys

try:
    print(os.getpgid(int(sys.argv[1])))
except (OSError, ValueError):
    raise SystemExit(1)
PY
  )" || return $?
  [ "${actual_pgid}" = "${group_leader_pid}" ]
}

gateway_terminate_launcher_job() {
  local gateway_pid="${1:?Gateway PID required}"
  local term_timeout_s="${2:?TERM timeout required}"
  local job_label="${3:-Gateway node}"

  if gateway_launcher_job_is_active "${gateway_pid}"; then
    kill "${gateway_pid}" 2>/dev/null || true
    if ! gateway_wait_for_job_exit "${gateway_pid}" "${term_timeout_s}"; then
      echo "migrate-edge: ${job_label} did not exit after SIGTERM; sending SIGKILL" >&2
      gateway_launcher_job_is_active "${gateway_pid}" && \
        kill -KILL "${gateway_pid}" 2>/dev/null || true
      if ! gateway_wait_for_job_exit "${gateway_pid}" 5; then
        echo "migrate-edge: launcher-owned ${job_label} PID survived SIGKILL: ${gateway_pid}" >&2
        return 1
      fi
    fi
  fi
  # The exact job is no longer active, so this reap cannot block.
  wait "${gateway_pid}" 2>/dev/null || true
}

gateway_terminate_launcher_group() {
  local group_leader_pid="${1:?group leader PID required}"
  local forwarded_signal="${2:?forwarded signal required}"
  local term_timeout_s="${3:?signal timeout required}"
  local job_label="${4:-validator process group}"
  local deadline now remaining_ms sleep_ms graceful_rc=0

  [[ "${group_leader_pid}" =~ ^[1-9][0-9]*$ ]] || return 1
  case "${forwarded_signal}" in INT | TERM) ;; *) return 1 ;; esac
  gateway_job_leads_process_group "${group_leader_pid}" || {
    echo "migrate-edge: ${job_label} is not led by PID ${group_leader_pid}" >&2
    return 1
  }

  if kill -"${forwarded_signal}" -- "-${group_leader_pid}" 2>/dev/null; then
    if now="$(migration_monotonic_millis)"; then
      deadline=$((now + term_timeout_s * 1000))
      while gateway_launcher_job_is_active "${group_leader_pid}"; do
        now="$(migration_monotonic_millis)" || { graceful_rc=$?; break; }
        remaining_ms=$((deadline - now))
        [ "${remaining_ms}" -gt 0 ] || break
        sleep_ms=100
        [ "${sleep_ms}" -le "${remaining_ms}" ] || sleep_ms="${remaining_ms}"
        sleep "$(migration_millis_as_seconds "${sleep_ms}")"
      done
    else
      graceful_rc=$?
    fi
  else
    graceful_rc=$?
  fi
  # SYSCOIN: the ready validator leader cannot exit voluntarily; while this
  # exact Bash job is live, its PGID cannot be recycled under the group KILL.
  gateway_launcher_job_is_active "${group_leader_pid}" || {
    wait "${group_leader_pid}" 2>/dev/null || true
    echo "migrate-edge: ${job_label} lost its owned leader before SIGKILL" >&2
    return 1
  }
  echo "migrate-edge: ${job_label} did not exit after SIG${forwarded_signal}; sending SIGKILL" >&2
  kill -KILL -- "-${group_leader_pid}" 2>/dev/null || return 1
  if ! gateway_wait_for_job_exit "${group_leader_pid}" 5; then
    echo "migrate-edge: ${job_label} leader survived SIGKILL: ${group_leader_pid}" >&2
    return 1
  fi
  wait "${group_leader_pid}" 2>/dev/null || true
  [ "${graceful_rc}" -eq 0 ] || \
    echo "migrate-edge: ${job_label} graceful shutdown failed; forced cleanup completed" >&2
  return 0
}

gateway_kill_completed_validator_group() {
  local group_leader_pid="${1:?group leader PID required}"
  gateway_job_leads_process_group "${group_leader_pid}" || return 1
  # SYSCOIN: the command is complete, so atomically kill its held leader and
  # every same-group straggler before reaping and releasing the PGID identity.
  kill -KILL -- "-${group_leader_pid}" 2>/dev/null || return 1
  gateway_wait_for_job_exit "${group_leader_pid}" 5 || return 1
  wait "${group_leader_pid}" 2>/dev/null || true
}

gateway_clear_validator_control_dir() {
  local control_dir="${GATEWAY_MIGRATION_VALIDATOR_CONTROL_DIR:-}"
  [ -n "${control_dir}" ] || return 0
  rm -f -- "${control_dir}/go" "${control_dir}/ready" \
    "${control_dir}/go.tmp" "${control_dir}/ready.tmp" "${control_dir}/status" \
    "${control_dir}/status.tmp"
  rmdir "${control_dir}" 2>/dev/null || true
  GATEWAY_MIGRATION_VALIDATOR_CONTROL_DIR=""
}

cleanup_gateway_for_migration_on_exit() {
  # SYSCOIN: do not let a repeated signal interrupt bounded child cleanup.
  trap '' INT TERM
  stop_gateway_for_migration || true
}

handle_gateway_migration_interrupt() {
  trap '' INT TERM
  GATEWAY_MIGRATION_CANCEL_SIGNAL=INT
  # SYSCOIN: repair's scoped EXIT handler performs cleanup, then atomically
  # blocks the checkpoint; ordinary launchers retain direct signal cleanup.
  [ "${GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND}" = true ] && exit 130
  trap - EXIT
  cleanup_gateway_for_migration_on_exit
  exit 130
}

handle_gateway_migration_terminate() {
  trap '' INT TERM
  GATEWAY_MIGRATION_CANCEL_SIGNAL=TERM
  [ "${GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND}" = true ] && exit 143
  trap - EXIT
  cleanup_gateway_for_migration_on_exit
  exit 143
}

gateway_dispatch_migration_signal() {
  local received_signal="${1:?signal required}"
  [ -n "${pending_signal:-}" ] || pending_signal="${received_signal}"
  trap '' INT TERM
  case "${pending_signal}" in
  INT) handle_gateway_migration_interrupt ;;
  TERM) handle_gateway_migration_terminate ;;
  esac
}

install_gateway_migration_cleanup_traps() {
  trap cleanup_gateway_for_migration_on_exit EXIT
  trap handle_gateway_migration_interrupt INT
  trap handle_gateway_migration_terminate TERM
}

migration_wait_for_owned_job() {
  local owned_pid="${1:?owned PID required}" wait_rc=0
  GATEWAY_MIGRATION_FOREGROUND_PID="${owned_pid}"
  wait "${owned_pid}" || wait_rc=$?
  GATEWAY_MIGRATION_FOREGROUND_PID=""
  return "${wait_rc}"
}

migration_interruptible_sleep() {
  sleep "${1:?sleep duration required}" &
  migration_wait_for_owned_job "$!"
}

run_gateway_repair_validator_in_owned_group() {
  local had_monitor=false group_leader_pid pending_signal="" validation_rc=0
  local control_dir ready_checks=0 result_kind result_line setup_cleanup_rc=0
  case "$-" in *m*) had_monitor=true ;; esac

  control_dir="$(mktemp -d "${TMPDIR:-/tmp}/gateway-validator.XXXXXX")" || exit $?
  chmod 700 "${control_dir}" || {
    rmdir "${control_dir}" 2>/dev/null || true
    exit 1
  }
  # SYSCOIN: close the fork/ownership publication window. A parent-only signal
  # is replayed through the normal handler only after the exact group is known.
  trap '[ -n "${pending_signal}" ] || pending_signal=INT' INT
  trap '[ -n "${pending_signal}" ] || pending_signal=TERM' TERM
  set -m
  (
    local child_rc=0 child_exit_rc
    readonly GATEWAY_VALIDATOR_CHILD_CONTROL_DIR="${control_dir}"
    set +e
    # SYSCOIN: disable nested job-control groups so every validator descendant
    # remains under this one parent-owned PGID.
    set +m
    validator_publish_and_hold() {
      local result_kind="${1:?result kind required}" result_rc="${2:?result code required}"
      trap - EXIT
      trap '' INT TERM
      if ! printf '%s:%s\n' "${result_kind}" "${result_rc}" > "${GATEWAY_VALIDATOR_CHILD_CONTROL_DIR}/status.tmp" || \
        ! mv "${GATEWAY_VALIDATOR_CHILD_CONTROL_DIR}/status.tmp" "${GATEWAY_VALIDATOR_CHILD_CONTROL_DIR}/status"; then
        # SYSCOIN: only a child-authenticated PGID may be signaled here. Before
        # that handshake, wake the repair parent to perform exact cleanup.
        if [ -n "${GATEWAY_VALIDATOR_CHILD_PGID:-}" ]; then
          if ! kill -KILL -- "-${GATEWAY_VALIDATOR_CHILD_PGID}" 2>/dev/null; then
            kill -TERM "$$" 2>/dev/null || true
          fi
        else
          kill -TERM "$$" 2>/dev/null || true
        fi
      fi
      while :; do sleep 3600; done
    }
    validator_fatal_exit() {
      child_exit_rc=$?
      validator_publish_and_hold fatal "${child_exit_rc}"
    }
    trap validator_fatal_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    : > "${control_dir}/ready.tmp" && mv "${control_dir}/ready.tmp" "${control_dir}/ready"
    while [ ! -f "${control_dir}/go" ]; do sleep 0.01; done
    child_group_pgid=""
    IFS= read -r child_group_pgid < "${control_dir}/go" || \
      validator_publish_and_hold fatal 1
    actual_child_pgid="$(python3 - <<'PY'
import os
print(os.getpgrp())
PY
    )" || validator_publish_and_hold fatal 1
    [[ "${child_group_pgid}" =~ ^[1-9][0-9]*$ ]] && \
      [ "${child_group_pgid}" = "${actual_child_pgid}" ] || \
      validator_publish_and_hold fatal 1
    readonly GATEWAY_VALIDATOR_CHILD_PGID="${child_group_pgid}"
    GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND=false
    "$@" || child_rc=$?
    trap - EXIT
    validator_publish_and_hold return "${child_rc}"
  ) &
  group_leader_pid=$!
  [ "${had_monitor}" = true ] || set +m

  while [ ! -f "${control_dir}/ready" ]; do
    if ! gateway_launcher_job_is_active "${group_leader_pid}"; then
      wait "${group_leader_pid}" || validation_rc=$?
      break
    fi
    [ "${ready_checks}" -lt 500 ] || break
    ready_checks=$((ready_checks + 1))
    sleep 0.01
  done
  if [ ! -f "${control_dir}/ready" ] || ! gateway_job_leads_process_group "${group_leader_pid}"; then
    GATEWAY_MIGRATION_VALIDATOR_CONTROL_DIR="${control_dir}"
    if gateway_job_leads_process_group "${group_leader_pid}"; then
      GATEWAY_MIGRATION_VALIDATOR_PID="${group_leader_pid}"
      gateway_kill_completed_validator_group "${group_leader_pid}" || setup_cleanup_rc=$?
      [ "${setup_cleanup_rc}" -ne 0 ] || GATEWAY_MIGRATION_VALIDATOR_PID=""
    else
      # SYSCOIN: before PGID authentication the held child has not received
      # the go token. KILL only that exact job, then bound and reap its exit.
      GATEWAY_MIGRATION_FOREGROUND_PID="${group_leader_pid}"
      if gateway_launcher_job_is_active "${group_leader_pid}"; then
        kill -KILL "${group_leader_pid}" 2>/dev/null || setup_cleanup_rc=$?
        [ "${setup_cleanup_rc}" -ne 0 ] || \
          gateway_wait_for_job_exit "${group_leader_pid}" 5 || setup_cleanup_rc=$?
      fi
      if [ "${setup_cleanup_rc}" -eq 0 ]; then
        wait "${group_leader_pid}" 2>/dev/null || true
        GATEWAY_MIGRATION_FOREGROUND_PID=""
      fi
    fi
    [ "${setup_cleanup_rc}" -ne 0 ] || gateway_clear_validator_control_dir
    trap 'gateway_dispatch_migration_signal INT' INT
    trap 'gateway_dispatch_migration_signal TERM' TERM
    [ -z "${pending_signal}" ] || gateway_dispatch_migration_signal "${pending_signal}"
    [ "${setup_cleanup_rc}" -eq 0 ] || exit 1
    trap handle_gateway_migration_interrupt INT
    trap handle_gateway_migration_terminate TERM
    exit 1
  fi

  GATEWAY_MIGRATION_VALIDATOR_PID="${group_leader_pid}"
  GATEWAY_MIGRATION_VALIDATOR_CONTROL_DIR="${control_dir}"
  # SYSCOIN: arm a first-signal dispatcher before replaying any signal latched
  # during fork, so neither signal type can be lost or supersede the first.
  trap 'gateway_dispatch_migration_signal INT' INT
  trap 'gateway_dispatch_migration_signal TERM' TERM
  [ -z "${pending_signal}" ] || gateway_dispatch_migration_signal "${pending_signal}"
  printf '%s\n' "${group_leader_pid}" > "${control_dir}/go.tmp" && \
    mv "${control_dir}/go.tmp" "${control_dir}/go" || exit $?

  while [ ! -s "${control_dir}/status" ]; do
    gateway_launcher_job_is_active "${group_leader_pid}" || {
      wait "${group_leader_pid}" || validation_rc=$?
      GATEWAY_MIGRATION_VALIDATOR_PID=""
      gateway_clear_validator_control_dir
      trap handle_gateway_migration_interrupt INT
      trap handle_gateway_migration_terminate TERM
      exit 1
    }
    migration_interruptible_sleep 0.05 || true
  done
  IFS= read -r result_line < "${control_dir}/status" || result_line=""
  result_kind="${result_line%%:*}"
  validation_rc="${result_line#*:}"
  case "${result_kind}" in return | fatal) ;; *) result_kind=fatal; validation_rc=1 ;; esac
  [[ "${validation_rc}" =~ ^([0-9]|[1-9][0-9]|1[0-9][0-9]|2[0-4][0-9]|25[0-5])$ ]] || \
    { result_kind=fatal; validation_rc=1; }
  [ "${result_kind}" != fatal ] || [ "${validation_rc}" -ne 0 ] || validation_rc=1
  gateway_kill_completed_validator_group "${group_leader_pid}" || exit $?
  GATEWAY_MIGRATION_VALIDATOR_PID=""
  gateway_clear_validator_control_dir
  if [ "${result_kind}" = fatal ]; then
    exit "${validation_rc}"
  fi
  trap handle_gateway_migration_interrupt INT
  trap handle_gateway_migration_terminate TERM
  [ "${validation_rc}" -ne 0 ] && return "${validation_rc}"
  return "${validation_rc}"
}

start_gateway_for_migration() {
  local start_script runner log_file start_timeout_s poll_interval_s chain_name owned_gateway_rpc startup_rc
  chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  start_script="${GATEWAY_DIR}/os-server-configs/${chain_name}/start-node.sh"
  [ -x "${start_script}" ] || gl_die "missing executable Gateway start script: ${start_script}"
  owned_gateway_rpc="$(gl_gateway_generated_rpc_url)" || return $?
  # This launcher owns a local node; keep all child helpers on that exact RPC.
  # Standalone helpers retain GATEWAY_RPC_URL support for split-host operation.
  export GATEWAY_RPC_URL="${owned_gateway_rpc}"
  set_gateway_runtime_l1_rpc_url || return $?
  if [ -n "${BITCOIN_DA_RPC_URL:-}" ]; then
    # SYSCOIN: checkpointed migration reruns may skip config materialization,
    # so re-check the DA wallet before Gateway tries to publish its first blob.
    gl_prepare_bitcoin_da_wallet || return $?
  fi

  if gateway_rpc_ready "${owned_gateway_rpc}"; then
    if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ]; then
      [ -n "${GATEWAY_NODE_PID}" ] && kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null || \
        gl_die "migrate-edge: launcher-owned Gateway PID is no longer alive: ${GATEWAY_NODE_PID:-<unset>}"
      gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}" || return $?
      gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" false "${owned_gateway_rpc}" || return $?
      gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}" || return $?
      kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null || \
        gl_die "migrate-edge: launcher-owned Gateway PID exited during re-attestation"
      echo "migrate-edge: reusing the Gateway node started by this launcher"
      print_gateway_prover_mode_hint
      return 0
    fi
    gl_die "migrate-edge: Gateway RPC is already reachable before this launcher started it; stop the stale/independent node or choose a fresh GATEWAY_OS_RPC_PORT"
  fi

  runner="${GL_DIR}/run-os-server-with-patched-zksync-os.sh"
  echo "migrate-edge: building the stamped Gateway node binary"
  bash "${runner}" "${chain_name}" -- build-prebuilt || return $?
  if gateway_rpc_ready "${owned_gateway_rpc}"; then
    gl_die "migrate-edge: Gateway RPC became reachable while preparing this launch; stop the stale/independent node or choose a fresh GATEWAY_OS_RPC_PORT"
  fi

  : "${GATEWAY_MIGRATION_GATEWAY_LOG:=${HOME}/gateway-migration-gateway-node.log}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT:=3600}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_POLL:=2}"
  # SYSCOIN: validate env-controlled values before Bash arithmetic expansion.
  start_timeout_s="$(normalize_migration_start_uint GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT "${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT}" 86400)" || return $?
  poll_interval_s="$(normalize_migration_start_uint GATEWAY_MIGRATION_GATEWAY_START_POLL "${GATEWAY_MIGRATION_GATEWAY_START_POLL}" 3600)" || return $?
  [ "${poll_interval_s}" -gt 0 ] || poll_interval_s=2

  log_file="${GATEWAY_MIGRATION_GATEWAY_LOG}"
  echo "migrate-edge: starting Gateway node via ${start_script} -> ${log_file}"
  # Do not let an orphaned Gateway node retain the launcher's lifecycle lock.
  nohup bash "${start_script}" 8>&- >"${log_file}" 2>&1 &
  GATEWAY_NODE_PID=$!
  export GATEWAY_RUNTIME_OWNER_PID="${GATEWAY_NODE_PID}"
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

  # SYSCOIN: enforce startup as a wall-clock deadline, including each RPC probe
  # and the final sleep, rather than multiplying a poll count by an interval.
  startup_rc=0
  gateway_wait_for_rpc_start \
    "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}" \
    "${start_timeout_s}" "${poll_interval_s}" || startup_rc=$?
  if [ "${startup_rc}" -ne 0 ]; then
    print_gateway_migration_log_excerpt "${log_file}"
    if [ "${startup_rc}" -eq 125 ]; then
      if gateway_replay_assertion_failed "${log_file}"; then
        gl_die "migrate-edge: Gateway node failed during replay with block_executor assertion mismatch. Stop the node, back up and move the complete ${GATEWAY_DIR}/os-server-configs/${chain_name}/db directory, then rerun."
      fi
      gl_die "migrate-edge: Gateway node exited before RPC came up; see ${log_file}"
    fi
    [ "${startup_rc}" -eq 124 ] || \
      gl_die "migrate-edge: Gateway startup monitor failed with exit code ${startup_rc}"
    gl_die "migrate-edge: Gateway RPC did not come up within ${start_timeout_s}s (see ${log_file})"
  fi

  gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}" || return $?
  # The first launcher-owned start is the only path allowed to create the
  # immutable block-0 deployment stamp. Every reuse merely verifies it.
  gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" true "${owned_gateway_rpc}" || return $?
  gl_assert_gateway_listener_owned_by_pid "${GATEWAY_NODE_PID}" "${owned_gateway_rpc}" || return $?
  kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null || \
    gl_die "migrate-edge: launcher-owned Gateway PID exited during first attestation"
  echo "migrate-edge: Gateway RPC is up"
  print_gateway_prover_mode_hint
}

stop_gateway_for_migration() {
  local chain_name config_path cleanup_rc stop_timeout_s
  cleanup_rc=0
  chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  config_path="${GATEWAY_DIR}/os-server-configs/${chain_name}/config.yaml"

  if [ -n "${GATEWAY_MIGRATION_VALIDATOR_PID}" ] || \
    [ -n "${GATEWAY_MIGRATION_FOREGROUND_PID}" ] || {
    [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ] && [ -n "${GATEWAY_NODE_PID}" ]
  }; then
    if stop_timeout_s="$(normalize_migration_start_uint \
      GATEWAY_MIGRATION_GATEWAY_STOP_TIMEOUT \
      "${GATEWAY_MIGRATION_GATEWAY_STOP_TIMEOUT:-10}" 300)"; then
      :
    else
      cleanup_rc=$?
      stop_timeout_s=10
    fi
  fi
  if [ -n "${GATEWAY_MIGRATION_VALIDATOR_PID}" ]; then
    if gateway_terminate_launcher_group \
      "${GATEWAY_MIGRATION_VALIDATOR_PID}" \
      "${GATEWAY_MIGRATION_CANCEL_SIGNAL:-TERM}" "${stop_timeout_s}" \
      "repair validator process group"; then
      GATEWAY_MIGRATION_VALIDATOR_PID=""
      gateway_clear_validator_control_dir
    else
      cleanup_rc=$?
    fi
  fi
  if [ -n "${GATEWAY_MIGRATION_FOREGROUND_PID}" ]; then
    # SYSCOIN: make the startup/poll sleep an exact job so the repair shell's
    # signal trap can stop both that wait and the launcher-owned node.
    if gateway_terminate_launcher_job \
      "${GATEWAY_MIGRATION_FOREGROUND_PID}" "${stop_timeout_s}" \
      "startup wait"; then
      GATEWAY_MIGRATION_FOREGROUND_PID=""
    else
      cleanup_rc=$?
    fi
  fi
  [ -n "${GATEWAY_MIGRATION_VALIDATOR_PID}" ] || \
    [ -n "${GATEWAY_MIGRATION_FOREGROUND_PID}" ] || \
    gateway_clear_validator_control_dir
  if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ] && [ -n "${GATEWAY_NODE_PID}" ]; then
    echo "migrate-edge: stopping Gateway node (pid ${GATEWAY_NODE_PID})"
    # SYSCOIN: never let a TERM-ignoring node block the cleanup path that
    # escalates it and locates any exact-config descendants left pre-exec.
    gateway_terminate_launcher_job \
      "${GATEWAY_NODE_PID}" "${stop_timeout_s}" || cleanup_rc=$?
  fi
  if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ]; then
    python3 - "${config_path}" <<'PY' || cleanup_rc=$?
import os
import shlex
import signal
import subprocess
import sys
import time

config_path = sys.argv[1]
current = {os.getpid(), os.getppid()}

# SYSCOIN: request only same-effective-UID PIDs here, then inspect argv with
# portable ps flags. Darwin's pgrep -a does not print argv as procps does. Match
# the executable and config option as exact tokens so a sibling config cannot
# be mistaken for the launcher-owned node.
result = subprocess.run(
    ["pgrep", "-u", str(os.geteuid()), "-f", "zksync-os-server"],
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
if result.returncode == 1:
    raise SystemExit(0)
if result.returncode != 0:
    raise SystemExit(
        f"pgrep failed while locating Gateway children: {result.stderr.strip()}"
    )


def uses_gateway_config(pid, rendered_argv):
    try:
        args = shlex.split(rendered_argv)
    except ValueError as error:
        raise SystemExit(f"cannot parse same-UID process {pid} argv: {error}")
    if not args or os.path.basename(args[0]) != "zksync-os-server":
        return False
    for index, arg in enumerate(args[1:], start=1):
        if arg == "--":
            break
        if arg == f"--config={config_path}":
            return True
        if arg == "--config" and index + 1 < len(args):
            if args[index + 1] == config_path:
                return True
    return False


pids = []
for line in result.stdout.splitlines():
    try:
        pid = int(line.strip())
    except ValueError:
        raise SystemExit(f"pgrep returned a malformed PID: {line!r}")
    if pid in current:
        continue
    argv = subprocess.run(
        ["ps", "-ww", "-p", str(pid), "-o", "command="],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if argv.returncode != 0:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        except PermissionError as error:
            raise SystemExit(f"cannot inspect same-UID process {pid}: {error}")
        raise SystemExit(
            f"ps failed while inspecting Gateway child {pid}: {argv.stderr.strip()}"
        )
    if uses_gateway_config(pid, argv.stdout.strip()):
        pids.append(pid)

if not pids:
    raise SystemExit(0)

print(f"migrate-edge: stopping Gateway node child processes {pids}")
for pid in pids:
    try:
        os.kill(pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass

deadline = time.monotonic() + 10
remaining = set(pids)
while remaining and time.monotonic() < deadline:
    for pid in list(remaining):
        try:
            os.kill(pid, 0)
        except (ProcessLookupError, PermissionError):
            remaining.remove(pid)
    if remaining:
        time.sleep(0.2)

for pid in remaining:
    try:
        os.kill(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass

deadline = time.monotonic() + 5
while remaining and time.monotonic() < deadline:
    for pid in list(remaining):
        try:
            os.kill(pid, 0)
        except (ProcessLookupError, PermissionError):
            remaining.remove(pid)
    if remaining:
        time.sleep(0.2)
if remaining:
    raise SystemExit(f"Gateway node child processes survived SIGKILL: {sorted(remaining)}")
PY
  fi
  GATEWAY_NODE_PID=""
  GATEWAY_STARTED_FOR_MIGRATION=false
  unset GATEWAY_RUNTIME_OWNER_PID
  return "${cleanup_rc}"
}

run_with_gateway_for_migration() {
  local start_rc=0 command_rc=0 cleanup_rc=0

  start_gateway_for_migration || start_rc=$?
  if [ "${start_rc}" -ne 0 ]; then
    stop_gateway_for_migration || cleanup_rc=$?
    [ "${GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND}" = true ] && \
      [ "${cleanup_rc}" -ne 0 ] && exit "${cleanup_rc}"
    return "${start_rc}"
  fi
  if [ "${GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND}" = true ]; then
    # SYSCOIN: repair keeps Gateway ownership in this shell while isolating the
    # validator command and its descendants in a separately owned PGID.
    run_gateway_repair_validator_in_owned_group "$@" || command_rc=$?
  else
    "$@" || command_rc=$?
  fi
  stop_gateway_for_migration || cleanup_rc=$?
  [ "${GATEWAY_MIGRATION_REPAIR_GROUP_COMMAND}" = true ] && \
    [ "${cleanup_rc}" -ne 0 ] && exit "${cleanup_rc}"
  if [ "${command_rc}" -ne 0 ]; then
    return "${command_rc}"
  fi
  return "${cleanup_rc}"
}
