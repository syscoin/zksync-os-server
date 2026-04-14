#!/usr/bin/env bash
set -euo pipefail

INSTANCE="${1:-}"
if [[ -z "${INSTANCE}" || ( "${INSTANCE}" != "gateway" && "${INSTANCE}" != "zksys" ) ]]; then
  echo "usage: $0 gateway|zksys" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE_HOST="${REMOTE_HOST:-ubuntu@88.198.25.188}"
SSH_KEY_PATH="${SSH_KEY_PATH:-/Users/jagsidhu/work/Documents/GitHub/PVUGC.prv}"
REMOTE_DIR="${REMOTE_DIR:-/home/ubuntu/gateway/ui/blockscout}"
PROJECT_NAME="blockscout-${INSTANCE}"

ssh -o StrictHostKeyChecking=accept-new -i "${SSH_KEY_PATH}" "${REMOTE_HOST}" "mkdir -p \"${REMOTE_DIR}\""
scp -o StrictHostKeyChecking=accept-new -i "${SSH_KEY_PATH}" -r \
  "${SCRIPT_DIR}/docker-compose.yml" \
  "${SCRIPT_DIR}/proxy" \
  "${SCRIPT_DIR}/envs" \
  "${REMOTE_HOST}:${REMOTE_DIR}/"

ssh -o StrictHostKeyChecking=accept-new -i "${SSH_KEY_PATH}" "${REMOTE_HOST}" \
  "cd \"${REMOTE_DIR}\" && docker compose --env-file \"envs/${INSTANCE}.env\" -p \"${PROJECT_NAME}\" up -d"
