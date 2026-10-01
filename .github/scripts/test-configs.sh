#!/usr/bin/env bash
set -euo pipefail

SERVER_LOGFILE=${SERVER_LOGFILE:-server.log}
TIMEOUT=${TIMEOUT:-120}
INTERVAL=${INTERVAL:-3}

COMPONENT_ONLY=false
if [ "$#" -eq 1 ] && [ "$1" = "--anvil-component-only" ]; then
  COMPONENT_ONLY=true
elif [ "$#" -ne 0 ]; then
  echo "usage: test-configs.sh [--anvil-component-only]" >&2
  exit 1
fi

# SYSCOIN: Never exercise a stale fixture against the app-bound V8 execution and verifier lane.
PENDING_FIXTURE=local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED
if [ "${COMPONENT_ONLY}" = false ] && [[ -f "${PENDING_FIXTURE}" ]]; then
  echo "error: canonical v32.0/V8 fixture regeneration is required: ${PENDING_FIXTURE}" >&2
  exit 1
fi

# name|state|config
# SYSCOIN: Only the canonical v32.0 identity may return here after its V8 fixture is regenerated.
CONFIGS=(
  "v32 default|local-chains/v32.0/l1-state.json|local-chains/v32.0/default/config.yaml"
)
if [ "${COMPONENT_ONLY}" = true ]; then
  # Explicit component coverage, not an escape hatch in the canonical fixture.
  python3 scripts/fixtures/verify-v32-component-fixture.py
  test -f ./anvil-component-bootstrap
  chmod a+x ./anvil-component-bootstrap
  CONFIGS=(
    "V32/V8 AnvilComponentOnly|local-chains/anvil-component-only/v32.0/l1-state.json|local-chains/anvil-component-only/v32.0/default/config.yaml"
  )
else
  chmod a+x ./zksync-os-server
fi

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi

  if [[ -n "${ANVIL_PID:-}" ]] && kill -0 "${ANVIL_PID}" 2>/dev/null; then
    kill "${ANVIL_PID}" 2>/dev/null || true
    wait "${ANVIL_PID}" 2>/dev/null || true
  fi

  rm -rf ./db
  SERVER_PID=""
  ANVIL_PID=""
}

on_error() {
  echo "❌ Smoke test failed"
  echo "Config name: ${CUR_NAME:-unknown}"
  echo "State:       ${CUR_STATE:-unknown}"
  echo "Config:      ${CUR_CONFIG:-unknown}"
  echo "---- ${SERVER_LOGFILE} ----"
  [[ -f "${SERVER_LOGFILE}" ]] && cat "${SERVER_LOGFILE}" || true
  cleanup
}

trap on_error ERR
trap cleanup EXIT

for entry in "${CONFIGS[@]}"; do
  cleanup

  IFS="|" read -r CUR_NAME CUR_STATE CUR_CONFIG <<< "${entry}"

  echo ""
  echo "============================================================"
  echo "▶ Running: ${CUR_NAME}"
  echo "  state:  ${CUR_STATE}"
  echo "  config: ${CUR_CONFIG}"
  echo "============================================================"

  : > "${SERVER_LOGFILE}"

  echo "Decompressing anvil state..."
  if [ "${COMPONENT_ONLY}" = true ]; then
    zstd --long=27 -dfk "${CUR_STATE}.zst"
  else
    gzip -dfk "${CUR_STATE}.gz"
  fi
  ANVIL_CLOCK_ARGS=()
  if [ "${COMPONENT_ONLY}" = true ]; then
    # Authenticate the actual decoded bytes before starting either component.
    python3 scripts/fixtures/verify-v32-component-fixture.py
    L1_TIMESTAMP=$(jq --stream -c \
      'select(length == 2 and .[0] == ["block", "timestamp"]) | .[1]' \
      "${CUR_STATE}" | python3 -c '
import json
import re
import sys

timestamp = json.load(sys.stdin)
if not isinstance(timestamp, str) or re.fullmatch(r"(?:0x[0-9a-fA-F]+|[0-9]+)", timestamp) is None:
    raise ValueError("L1 state timestamp must be a hexadecimal or decimal string")
value = int(timestamp[2:], 16) if timestamp.startswith("0x") else int(timestamp, 10)
if not 0 <= value <= 2**64 - 1:
    raise ValueError("L1 state timestamp is outside u64")
print(value)
'
    )
    # Match the registered bootstrap's sequencer offset to the replay clock.
    ANVIL_CLOCK_ARGS=(--timestamp "${L1_TIMESTAMP}")
  fi

  echo "Starting anvil..."
  anvil --load-state "${CUR_STATE}" "${ANVIL_CLOCK_ARGS[@]}" --port 8545 --block-time 0.25 --mixed-mining --slots-in-an-epoch 10 > anvil.log 2>&1 &
  ANVIL_PID=$!

  echo "Starting server..."
  if [ "${COMPONENT_ONLY}" = true ]; then
    # Normal config validation and server implementation, explicit test DA mock.
    # This is component startup/RPC/tx coverage, not production Core/DA coverage.
    COMPONENT_DEADLINE=$(( $(date +%s) + TIMEOUT + 60 ))
    SYSCOIN_ANVIL_COMPONENT_ONLY=31337 ./anvil-component-bootstrap "${CUR_CONFIG}" "${COMPONENT_DEADLINE}" > "${SERVER_LOGFILE}" 2>&1 &
  else
    ./zksync-os-server --config "local-chains/local_dev.yaml" --config "${CUR_CONFIG}" > "${SERVER_LOGFILE}" 2>&1 &
  fi
  SERVER_PID=$!

  RPC_PORT=$(yq -r '
    (
      (.rpc.address // "")
      | select(length > 0)
      // "0.0.0.0:3050"
    )
    | split(":") | .[-1]
  ' "${CUR_CONFIG}")

  echo "Waiting for server on port ${RPC_PORT}..."
  START_TIME=$(date +%s)

  while ! nc -z localhost "${RPC_PORT}"; do
    NOW=$(date +%s)
    ELAPSED=$((NOW - START_TIME))
    if [[ "${ELAPSED}" -ge "${TIMEOUT}" ]]; then
      echo "⏰ Timed out after ${TIMEOUT}s"
      cat "${SERVER_LOGFILE}"
      exit 1
    fi
    echo "Waiting... (${ELAPSED}s)"
    sleep "${INTERVAL}"
  done

  echo "✅ Server is up"

  TEST_PRIVATE_KEY=0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110
  FROM=0x36615cf349d7f6344891b1e7ca7c72883f5dc049
  TO=0x5A67EE02274D9Ec050d412b96fE810Be4D71e7A0

  echo "Sending test transaction..."
  MAX_RETRIES=5
  RETRY_DELAY=2  # seconds
  attempt=1
  while true; do
    cast balance --rpc-url "http://localhost:${RPC_PORT}" "${FROM}"
    if cast send \
      --private-key "${TEST_PRIVATE_KEY}" \
      --rpc-url "http://localhost:${RPC_PORT}" \
      "${TO}" \
      --value 10; then
      echo "✅ Test transaction succeeded!"
      break
    fi
    if [ "${attempt}" -ge "${MAX_RETRIES}" ]; then
      echo "❌ Test transaction failed after ${MAX_RETRIES} attempts!"
      cat "${SERVER_LOGFILE}"
      exit 1
    fi
    echo "⚠️  Cast send failed (attempt ${attempt}/${MAX_RETRIES}), retrying in ${RETRY_DELAY}s..."
    attempt=$((attempt + 1))
    sleep "${RETRY_DELAY}"
  done

  echo "✅ ${CUR_NAME} passed"
done

echo ""
echo "🎉 All configs passed successfully"
