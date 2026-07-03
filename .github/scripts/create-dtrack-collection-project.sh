#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

usage() {
  cat <<'EOF'
Usage:
  create-dtrack-collection-project.sh

Required env:
  DT_BASE_URL      Dependency Track base URL (e.g. https://dtrack.example.com)
  DT_API_KEY       Dependency Track API key
  PROJECT_NAME     Name for the collection project
  PROJECT_VERSION  Version string (e.g. main, v1.0.0)
  PARENT_UUID      UUID of the parent project
EOF
}

log() { printf '[%s] %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

validate_inputs() {
  # SYSCOIN: this value can come from workflow_dispatch input. Keep it Docker
  # tag compatible and reject JSON metacharacters before using the DTrack API key.
  [[ "${PROJECT_VERSION}" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]] || \
    die "PROJECT_VERSION must be a Docker/tag-safe identifier"

  [[ "${PARENT_UUID}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] || \
    die "PARENT_UUID must be a UUID"
}

project_payload() {
  jq -n \
    --arg name "${PROJECT_NAME}" \
    --arg version "${PROJECT_VERSION}" \
    --arg parent_uuid "${PARENT_UUID}" \
    '{
      name: $name,
      version: $version,
      classifier: "APPLICATION",
      active: true,
      isLatest: true,
      collectionLogic: "AGGREGATE_LATEST_VERSION_CHILDREN",
      parent: {uuid: $parent_uuid}
    }'
}

main() {
  : "${DT_BASE_URL:?Set DT_BASE_URL (e.g. https://dtrack.example.com)}"
  : "${DT_API_KEY:?Set DT_API_KEY}"
  : "${PROJECT_NAME:?Set PROJECT_NAME}"
  : "${PROJECT_VERSION:?Set PROJECT_VERSION}"
  : "${PARENT_UUID:?Set PARENT_UUID}"
  validate_inputs

  log "Creating collection project '${PROJECT_NAME}' ${PROJECT_VERSION} under parent ${PARENT_UUID}..."

  local http_response http_status payload response_body uuid
  payload="$(project_payload)"

  http_response=$(curl -s -w "\n%{http_code}" \
    -X PUT "${DT_BASE_URL}/api/v1/project" \
    -H "X-Api-Key: ${DT_API_KEY}" \
    -H "Content-Type: application/json" \
    -d "${payload}")

  http_status=$(printf '%s' "${http_response}" | tail -n1)
  response_body=$(printf '%s' "${http_response}" | head -n-1)

  if [[ "${http_status}" == "201" ]]; then
    log "Project created."
    uuid=$(printf '%s' "${response_body}" | jq -r '.uuid')
  elif [[ "${http_status}" == "409" ]]; then
    log "Project already exists, fetching existing project..."
    local existing
    # SYSCOIN: avoid the paginated project-list endpoint here; exact lookup by
    # name and version prevents matching a child project that shares the version.
    existing=$(curl -sf --get \
      --data-urlencode "name=${PROJECT_NAME}" \
      --data-urlencode "version=${PROJECT_VERSION}" \
      "${DT_BASE_URL}/api/v1/project/lookup" \
      -H "X-Api-Key: ${DT_API_KEY}")
    uuid=$(printf '%s' "${existing}" | jq -r '.uuid // empty')
    [[ -z "${uuid}" ]] && die "Could not locate existing project '${PROJECT_NAME}' ${PROJECT_VERSION}"
  else
    log "Unexpected HTTP ${http_status}:"
    printf '%s\n' "${response_body}" >&2
    exit 1
  fi

  local project name version
  project=$(curl -sf \
    -X GET "${DT_BASE_URL}/api/v1/project/${uuid}" \
    -H "X-Api-Key: ${DT_API_KEY}")

  name=$(printf '%s' "${project}"    | jq -r '.name')
  version=$(printf '%s' "${project}" | jq -r '.version // "n/a"')

  log "Project UUID:    ${uuid}"
  log "Project name:    ${name}"
  log "Project version: ${version}"
}

main "$@"
