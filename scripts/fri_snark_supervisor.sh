#!/usr/bin/env bash
set -euo pipefail

# Quota-based GPU supervisor:
# - FRI phase: run one-shot FRI workers on all configured GPUs in parallel.
# - SNARK phase: run one-shot SNARK worker on SNARK_GPU.
# - Persist state so restarts continue the same FRI/SNARK flow.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVER_ROOT="${PROVER_ROOT:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

SEQUENCER_URLS="${SEQUENCER_URLS:-http://127.0.0.1:3124}"
APP_BIN_PATH="${APP_BIN_PATH:-${PROVER_ROOT}/multiblock_batch.bin}"
TRUSTED_SETUP_FILE="${TRUSTED_SETUP_FILE:-${PROVER_ROOT}/crs/setup_compact.key}"
OUTPUT_DIR="${OUTPUT_DIR:-${PROVER_ROOT}/outputs}"

FRI_BIN="${FRI_BIN:-${PROVER_ROOT}/target/release/zksync_os_fri_prover}"
SNARK_BIN="${SNARK_BIN:-${PROVER_ROOT}/target/release/zksync_os_snark_prover}"

GPU_LIST="${GPU_LIST:-0,1,2}"
SNARK_GPU="${SNARK_GPU:-}"

FRI_QUOTA="${FRI_QUOTA:-100}"
FRI_WORKER_TIMEOUT_SECONDS="${FRI_WORKER_TIMEOUT_SECONDS:-1800}"
SNARK_WORKER_TIMEOUT_SECONDS="${SNARK_WORKER_TIMEOUT_SECONDS:-900}"
SUPERVISOR_LOOP_SLEEP_SECONDS="${SUPERVISOR_LOOP_SLEEP_SECONDS:-1}"
FRI_POLL_INTERVAL_SECONDS="${FRI_POLL_INTERVAL_SECONDS:-1}"

FRI_CIRCUIT_LIMIT="${FRI_CIRCUIT_LIMIT:-10000}"
FRI_REQUEST_TIMEOUT_SECONDS="${FRI_REQUEST_TIMEOUT_SECONDS:-15}"
SNARK_REQUEST_TIMEOUT_SECONDS="${SNARK_REQUEST_TIMEOUT_SECONDS:-15}"
SNARK_DISABLE_ZK="${SNARK_DISABLE_ZK:-0}"

PROMETHEUS_BASE_PORT="${PROMETHEUS_BASE_PORT:-3220}"
PROVER_NAME_PREFIX="${PROVER_NAME_PREFIX:-quota_worker}"

STATE_DIR="${STATE_DIR:-${PROVER_ROOT}/.supervisor_state}"
STATE_FILE="${STATE_FILE:-${STATE_DIR}/fri_snark_state.env}"
LOCK_FILE="${LOCK_FILE:-${STATE_DIR}/supervisor.lock}"
RUN_LOG_DIR="${RUN_LOG_DIR:-${STATE_DIR}/run_logs}"

mkdir -p "${STATE_DIR}" "${RUN_LOG_DIR}" "${OUTPUT_DIR}"

exec 9>"${LOCK_FILE}"
if ! flock -n 9; then
  echo "another supervisor instance is already running (lock: ${LOCK_FILE})" >&2
  exit 1
fi

IFS=',' read -r -a GPUS <<< "${GPU_LIST}"
if [[ "${#GPUS[@]}" -eq 0 ]]; then
  echo "GPU_LIST resolved to empty set" >&2
  exit 1
fi
if [[ -z "${SNARK_GPU}" ]]; then
  SNARK_GPU="${GPUS[$((${#GPUS[@]} - 1))]}"
fi

if [[ ! -x "${FRI_BIN}" ]]; then
  echo "FRI binary not executable: ${FRI_BIN}" >&2
  exit 1
fi
if [[ ! -x "${SNARK_BIN}" ]]; then
  echo "SNARK binary not executable: ${SNARK_BIN}" >&2
  exit 1
fi

if [[ ! -f "${STATE_FILE}" ]]; then
  cat >"${STATE_FILE}" <<'EOF'
PHASE=fri
FRI_STREAK=0
SNARK_ATTEMPTS=0
SNARK_SUCCESSES=0
EOF
fi

load_state() {
  # shellcheck disable=SC1090
  source "${STATE_FILE}"
  PHASE="${PHASE:-fri}"
  FRI_STREAK="${FRI_STREAK:-0}"
  SNARK_ATTEMPTS="${SNARK_ATTEMPTS:-0}"
  SNARK_SUCCESSES="${SNARK_SUCCESSES:-0}"
}

save_state() {
  local tmp
  tmp="$(mktemp "${STATE_DIR}/state.XXXXXX")"
  {
    echo "PHASE=${PHASE}"
    echo "FRI_STREAK=${FRI_STREAK}"
    echo "SNARK_ATTEMPTS=${SNARK_ATTEMPTS}"
    echo "SNARK_SUCCESSES=${SNARK_SUCCESSES}"
  } >"${tmp}"
  mv "${tmp}" "${STATE_FILE}"
}

timestamp() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

run_fri_oneshot() {
  local gpu="$1"
  local result_file="$2"
  local run_log="${RUN_LOG_DIR}/fri_gpu${gpu}_$(date -u +%Y%m%dT%H%M%SZ).log"
  local prover_name="${PROVER_NAME_PREFIX}_fri_gpu${gpu}"
  local port=$((PROMETHEUS_BASE_PORT + gpu))
  local rc=0

  timeout "${FRI_WORKER_TIMEOUT_SECONDS}s" \
    env CUDA_VISIBLE_DEVICES="${gpu}" \
    "${FRI_BIN}" \
    --sequencer-urls "${SEQUENCER_URLS}" \
    --app-bin-path "${APP_BIN_PATH}" \
    --circuit-limit "${FRI_CIRCUIT_LIMIT}" \
    --iterations 1 \
    --request-timeout-secs "${FRI_REQUEST_TIMEOUT_SECONDS}" \
    --prometheus-port "${port}" \
    --prover-name "${prover_name}" \
    >"${run_log}" 2>&1 || rc=$?

  if [[ "${rc}" -eq 0 ]] && rg -q "Successfully submitted proof for batch number" "${run_log}"; then
    echo "1" >"${result_file}"
  else
    echo "0" >"${result_file}"
  fi
}

run_snark_oneshot() {
  local gpu="$1"
  local run_log="${RUN_LOG_DIR}/snark_gpu${gpu}_$(date -u +%Y%m%dT%H%M%SZ).log"
  local prover_name="${PROVER_NAME_PREFIX}_snark_gpu${gpu}"
  local port=$((PROMETHEUS_BASE_PORT + 100 + gpu))
  local disable_flag=()
  local rc=0

  if [[ "${SNARK_DISABLE_ZK}" == "1" ]]; then
    disable_flag=(--disable-zk)
  fi

  timeout "${SNARK_WORKER_TIMEOUT_SECONDS}s" \
    env CUDA_VISIBLE_DEVICES="${gpu}" \
    "${SNARK_BIN}" run-prover \
    --sequencer-urls "${SEQUENCER_URLS}" \
    --binary-path "${APP_BIN_PATH}" \
    --output-dir "${OUTPUT_DIR}" \
    --trusted-setup-file "${TRUSTED_SETUP_FILE}" \
    --iterations 1 \
    --request-timeout-secs "${SNARK_REQUEST_TIMEOUT_SECONDS}" \
    --prometheus-port "${port}" \
    --prover-name "${prover_name}" \
    "${disable_flag[@]}" \
    >"${run_log}" 2>&1 || rc=$?

  if [[ "${rc}" -eq 0 ]] && rg -q "Successfully submitted SNARK proof for batches" "${run_log}"; then
    return 0
  fi
  return 1
}

echo "$(timestamp) supervisor_start phase=fri quota=${FRI_QUOTA} gpus=${GPU_LIST} snark_gpu=${SNARK_GPU}"

while true; do
  load_state

  if [[ "${PHASE}" == "fri" ]]; then
    declare -A FRI_PIDS=()
    declare -A FRI_RESULT_FILES=()
    quota_reached=0

    spawn_fri_worker() {
      local gpu="$1"
      local result_file
      result_file="$(mktemp "${STATE_DIR}/fri_result_gpu${gpu}.XXXXXX")"
      FRI_RESULT_FILES["${gpu}"]="${result_file}"
      run_fri_oneshot "${gpu}" "${result_file}" &
      FRI_PIDS["${gpu}"]="$!"
      echo "$(timestamp) fri_worker_started gpu=${gpu} pid=${FRI_PIDS[${gpu}]}"
    }

    collect_fri_result() {
      local gpu="$1"
      local pid="${FRI_PIDS[${gpu}]}"
      local result_file="${FRI_RESULT_FILES[${gpu}]}"
      local success=0

      wait "${pid}" || true
      if [[ -f "${result_file}" ]] && [[ "$(cat "${result_file}")" == "1" ]]; then
        success=1
      fi
      rm -f "${result_file}"
      unset FRI_PIDS["${gpu}"]
      unset FRI_RESULT_FILES["${gpu}"]
      echo "${success}"
    }

    # Initial fill for all available GPUs.
    for gpu in "${GPUS[@]}"; do
      spawn_fri_worker "${gpu}"
    done

    # Continuous refill loop: refill each GPU immediately when it finishes.
    while true; do
      active=0
      for gpu in "${GPUS[@]}"; do
        pid="${FRI_PIDS[${gpu}]-}"
        if [[ -z "${pid}" ]]; then
          continue
        fi

        if kill -0 "${pid}" 2>/dev/null; then
          active=1
          continue
        fi

        success="$(collect_fri_result "${gpu}")"
        if [[ "${success}" == "1" ]]; then
          FRI_STREAK=$((FRI_STREAK + 1))
          echo "$(timestamp) fri_job_done gpu=${gpu} result=success fri_streak=${FRI_STREAK}"
          if [[ "${FRI_STREAK}" -ge "${FRI_QUOTA}" ]]; then
            PHASE="snark"
            quota_reached=1
            save_state
            echo "$(timestamp) fri_quota_reached fri_streak=${FRI_STREAK} draining_inflight=1 next_phase=${PHASE}"
          else
            save_state
          fi
        else
          echo "$(timestamp) fri_job_done gpu=${gpu} result=no_job_or_fail fri_streak=${FRI_STREAK}"
        fi

        # Refill immediately unless we are switching to SNARK.
        if [[ "${quota_reached}" -eq 0 ]]; then
          spawn_fri_worker "${gpu}"
          active=1
        fi
      done

      # If quota reached, wait for all in-flight FRI workers to complete, then switch.
      if [[ "${quota_reached}" -eq 1 ]]; then
        inflight=0
        for gpu in "${GPUS[@]}"; do
          if [[ -n "${FRI_PIDS[${gpu}]-}" ]]; then
            inflight=1
            break
          fi
        done
        if [[ "${inflight}" -eq 0 ]]; then
          break
        fi
      fi

      # Should not happen in normal operation, but keep scheduler alive.
      if [[ "${active}" -eq 0 ]] && [[ "${quota_reached}" -eq 0 ]]; then
        for gpu in "${GPUS[@]}"; do
          if [[ -z "${FRI_PIDS[${gpu}]-}" ]]; then
            spawn_fri_worker "${gpu}"
          fi
        done
      fi

      sleep "${FRI_POLL_INTERVAL_SECONDS}"
    done
  else
    SNARK_ATTEMPTS=$((SNARK_ATTEMPTS + 1))
    if run_snark_oneshot "${SNARK_GPU}"; then
      SNARK_SUCCESSES=$((SNARK_SUCCESSES + 1))
      echo "$(timestamp) snark_round result=success snark_attempts=${SNARK_ATTEMPTS} snark_successes=${SNARK_SUCCESSES}"
    else
      echo "$(timestamp) snark_round result=none_or_timeout snark_attempts=${SNARK_ATTEMPTS} snark_successes=${SNARK_SUCCESSES}"
    fi

    # Restart a fresh FRI quota window after each SNARK attempt.
    FRI_STREAK=0
    PHASE="fri"
    save_state
  fi

  sleep "${SUPERVISOR_LOOP_SLEEP_SECONDS}"
done

