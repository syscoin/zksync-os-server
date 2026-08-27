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
    headers={
        "Content-Type": "application/json",
        # SYSCOIN: Tanenbaum RPC rejects Python's default urllib user agent.
        "User-Agent": "gateway-launch/1.0",
    },
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
gateway_rpc_ready() {
  local gateway_rpc="${1:-$(gl_gateway_runtime_rpc_url)}" block_no
  block_no="$(json_rpc_hex_to_dec "${gateway_rpc}" "eth_blockNumber" 2>/dev/null || true)"
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

start_gateway_for_migration() {
  local start_script log_file i start_timeout_s poll_interval_s max_checks chain_name owned_gateway_rpc
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
      gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" false "${owned_gateway_rpc}" || return $?
      kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null || \
        gl_die "migrate-edge: launcher-owned Gateway PID exited during re-attestation"
      echo "migrate-edge: reusing the Gateway node started by this launcher"
      print_gateway_prover_mode_hint
      return 0
    fi
    gl_die "migrate-edge: Gateway RPC is already reachable before this launcher started it; stop the stale/independent node or choose a fresh GATEWAY_OS_RPC_PORT"
  fi

  : "${GATEWAY_MIGRATION_GATEWAY_LOG:=${HOME}/gateway-migration-gateway-node.log}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT:=3600}"
  : "${GATEWAY_MIGRATION_GATEWAY_START_POLL:=2}"
  # SYSCOIN: validate env-controlled values before Bash arithmetic expansion.
  start_timeout_s="$(normalize_migration_start_uint GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT "${GATEWAY_MIGRATION_GATEWAY_START_TIMEOUT}" 86400)" || return $?
  poll_interval_s="$(normalize_migration_start_uint GATEWAY_MIGRATION_GATEWAY_START_POLL "${GATEWAY_MIGRATION_GATEWAY_START_POLL}" 3600)" || return $?
  [ "${poll_interval_s}" -gt 0 ] || poll_interval_s=2
  max_checks=$((start_timeout_s / poll_interval_s))
  [ "${max_checks}" -gt 0 ] || max_checks=1

  log_file="${GATEWAY_MIGRATION_GATEWAY_LOG}"
  echo "migrate-edge: starting Gateway node via ${start_script} -> ${log_file}"
  # Do not let an orphaned Gateway node retain the launcher's lifecycle lock.
  nohup bash "${start_script}" 8>&- >"${log_file}" 2>&1 &
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
    if ! kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null; then
      print_gateway_migration_log_excerpt "${log_file}"
      if gateway_replay_assertion_failed "${log_file}"; then
        gl_die "migrate-edge: Gateway node failed during replay with block_executor assertion mismatch. This usually means stale gateway DB state from a prior incompatible run. Remove ${GATEWAY_DIR}/os-server-configs/${chain_name}/db and rerun."
      fi
      gl_die "migrate-edge: Gateway node exited before RPC came up; see ${log_file}"
    fi
    if gateway_rpc_ready "${owned_gateway_rpc}"; then
      # The first launcher-owned start is the only path allowed to create the
      # immutable block-0 deployment stamp. Every reuse merely verifies it.
      gl_assert_gateway_runtime_identity "${GATEWAY_NODE_PID}" true "${owned_gateway_rpc}" || return $?
      kill -0 "${GATEWAY_NODE_PID}" 2>/dev/null || \
        gl_die "migrate-edge: launcher-owned Gateway PID exited during first attestation"
      echo "migrate-edge: Gateway RPC is up"
      print_gateway_prover_mode_hint
      return 0
    fi
    sleep "${poll_interval_s}"
  done
  print_gateway_migration_log_excerpt "${log_file}"
  gl_die "migrate-edge: Gateway RPC did not come up within ${start_timeout_s}s (see ${log_file})"
}

stop_gateway_for_migration() {
  local chain_name config_path cleanup_rc
  cleanup_rc=0
  chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  config_path="${GATEWAY_DIR}/os-server-configs/${chain_name}/config.yaml"

  if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ] && [ -n "${GATEWAY_NODE_PID}" ]; then
    echo "migrate-edge: stopping Gateway node (pid ${GATEWAY_NODE_PID})"
    kill "${GATEWAY_NODE_PID}" 2>/dev/null || true
    wait "${GATEWAY_NODE_PID}" 2>/dev/null || true
  fi
  if [ "${GATEWAY_STARTED_FOR_MIGRATION}" = true ]; then
    python3 - "${config_path}" <<'PY' || cleanup_rc=$?
import os
import signal
import subprocess
import sys
import time

config_path = sys.argv[1]
needle = f"zksync-os-server --config {config_path}"
current = {os.getpid(), os.getppid()}

# SYSCOIN: request only same-effective-UID PIDs here, then inspect argv with
# portable ps flags. Darwin's pgrep -a does not print argv as procps does.
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
    if needle in argv.stdout.strip():
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
  return "${cleanup_rc}"
}

run_with_gateway_for_migration() {
  local start_rc=0 command_rc=0 cleanup_rc=0

  start_gateway_for_migration || start_rc=$?
  if [ "${start_rc}" -ne 0 ]; then
    stop_gateway_for_migration || true
    return "${start_rc}"
  fi
  "$@" || command_rc=$?
  stop_gateway_for_migration || cleanup_rc=$?
  if [ "${command_rc}" -ne 0 ]; then
    return "${command_rc}"
  fi
  return "${cleanup_rc}"
}
