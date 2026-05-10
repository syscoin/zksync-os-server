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
REMOTE_DIR_B64="$(printf '%s' "${REMOTE_DIR}" | base64 | tr -d '\n')"

tar -C "${SCRIPT_DIR}" -cf - docker-compose.yml proxy "envs/${INSTANCE}.env" | \
  ssh -o StrictHostKeyChecking=accept-new -i "${SSH_KEY_PATH}" "${REMOTE_HOST}" \
    "REMOTE_DIR_B64=${REMOTE_DIR_B64} INSTANCE=${INSTANCE} bash -c '
set -euo pipefail

remote_dir=\"\$(printf %s \"\${REMOTE_DIR_B64}\" | base64 -d)\"
instance=\"\${INSTANCE}\"
tmp_dir=\"\$(mktemp -d)\"
cleanup() {
  rm -rf \"\${tmp_dir}\"
}
trap cleanup EXIT

tar -C \"\${tmp_dir}\" -xf -

mkdir -p \"\${remote_dir}/envs\"
cp \"\${tmp_dir}/docker-compose.yml\" \"\${remote_dir}/docker-compose.yml\"
rm -rf \"\${remote_dir}/proxy\"
cp -R \"\${tmp_dir}/proxy\" \"\${remote_dir}/proxy\"
cp \"\${tmp_dir}/envs/\${instance}.env\" \"\${remote_dir}/envs/\${instance}.env\"
'"

ssh -o StrictHostKeyChecking=accept-new -i "${SSH_KEY_PATH}" "${REMOTE_HOST}" bash -s -- \
  "${REMOTE_DIR}" "${INSTANCE}" "${PROJECT_NAME}" <<'REMOTE_SCRIPT'
set -euo pipefail

remote_dir="$1"
instance="$2"
project_name="$3"

cd "${remote_dir}"

secrets_file="envs/${instance}.secrets.env"
if [[ ! -f "${secrets_file}" ]]; then
  if docker volume inspect "${project_name}_db-data" >/dev/null 2>&1; then
    echo "Existing Blockscout DB volume found but ${secrets_file} is missing." >&2
    echo "Create ${secrets_file} with the current POSTGRES_PASSWORD before deploying." >&2
    exit 1
  fi

  umask 077
  {
    printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -base64 24 | tr '/+' '_-' | tr -d '\n')"
    printf 'SECRET_KEY_BASE=%s\n' "$(openssl rand -base64 48 | tr '/+' '_-' | tr -d '\n')"
  } > "${secrets_file}"
fi

docker compose \
  --env-file "envs/${instance}.env" \
  --env-file "${secrets_file}" \
  -p "${project_name}" \
  up -d
REMOTE_SCRIPT
