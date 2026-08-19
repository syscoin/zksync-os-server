#!/usr/bin/env bash
# Explicit, network-free independent conformance gate for the SP 800-230 IPD
# SLH-DSA-SHA2-128-24 vectors. This is intentionally not a default workspace
# test because the authoritative external source must be supplied separately.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

EXPECTED_SOURCE_COMMIT="174c02e42257f95c210963272877c49dbb50070f"
EXPECTED_PATCH_SHA256="21190d9ea74828af13e305e93ee344b4873a6995a724d5d62a43e65024041e14"
EXPECTED_HARNESS_SHA256="1b50f6380562da9375921daf7bb80938fddfec30fab7f2f6094717ee2c035832"
EXPECTED_DRIVER_SHA256="adcc1de3ce18608ed3e35cedb34ea5b5bcec4dea1a9fdaa55d06dbb1cfb2a195"
EXPECTED_LEGACY_VECTOR_SHA256="ba33ddf2addd6393e727b2c299e10ec2b551bfaf3dc7fe23373e7df00dfa6385"
EXPECTED_CANONICAL_VECTOR_SHA256="133fe2a7f6a4218286ac501da8c2b875423510d7717606bba948ffc9219a2272"

PATCH_FILE="${SCRIPT_DIR}/slhdsa-c-sp800-230-ipd.patch"
HARNESS_FILE="${SCRIPT_DIR}/verify.c"
DRIVER_FILE="${SCRIPT_DIR}/verify_vectors.py"
LEGACY_VECTOR="${REPO_ROOT}/contracts/test/vectors/slh_dsa_sha2_128_24_kat.json"
CANONICAL_VECTOR="${REPO_ROOT}/contracts/test/vectors/slh_dsa_sha2_128_24_sp800_230_ipd_counter0.json"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "error: sha256sum or shasum is required" >&2
    return 1
  fi
}

expect_hash() {
  local path="$1"
  local expected="$2"
  local label="$3"
  local actual
  actual="$(sha256_file "${path}")"
  if [[ "${actual}" != "${expected}" ]]; then
    echo "error: ${label} SHA-256 mismatch" >&2
    echo "  expected: ${expected}" >&2
    echo "  actual:   ${actual}" >&2
    return 1
  fi
}

usage() {
  cat >&2 <<'EOF'
usage: tools/slh-dsa-conformance/run.sh /path/to/slhdsa-c
   or: SLHDSA_C_PATH=/path/to/slhdsa-c tools/slh-dsa-conformance/run.sh

The source checkout must be clean and exactly at:
  pq-code-package/slhdsa-c@174c02e42257f95c210963272877c49dbb50070f

This runner never fetches from the network and never mutates the supplied checkout.
EOF
}

if (( $# > 1 )); then
  usage
  exit 2
fi
SOURCE_PATH="${1:-${SLHDSA_C_PATH:-}}"
if [[ -z "${SOURCE_PATH}" ]]; then
  usage
  exit 2
fi
if [[ ! -d "${SOURCE_PATH}/.git" ]]; then
  echo "error: not a git checkout: ${SOURCE_PATH}" >&2
  exit 2
fi
SOURCE_PATH="$(cd "${SOURCE_PATH}" && pwd)"

for required in git awk; do
  if ! command -v "${required}" >/dev/null 2>&1; then
    echo "error: required command not found: ${required}" >&2
    exit 2
  fi
done
CC_BIN="${CC:-cc}"
PYTHON_BIN="${PYTHON:-python3}"
if ! command -v "${CC_BIN}" >/dev/null 2>&1; then
  echo "error: C compiler not found: ${CC_BIN}" >&2
  exit 2
fi
if ! command -v "${PYTHON_BIN}" >/dev/null 2>&1; then
  echo "error: Python interpreter not found: ${PYTHON_BIN}" >&2
  exit 2
fi

# SYSCOIN: Bind the independent implementation, local adaptation, harness, and
# both fixtures before compiling or executing any cryptographic code.
actual_commit="$(git -C "${SOURCE_PATH}" rev-parse HEAD)"
if [[ "${actual_commit}" != "${EXPECTED_SOURCE_COMMIT}" ]]; then
  echo "error: slhdsa-c commit mismatch" >&2
  echo "  expected: ${EXPECTED_SOURCE_COMMIT}" >&2
  echo "  actual:   ${actual_commit}" >&2
  exit 1
fi
if [[ -n "$(git -C "${SOURCE_PATH}" status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "error: slhdsa-c checkout is not clean" >&2
  exit 1
fi
expect_hash "${PATCH_FILE}" "${EXPECTED_PATCH_SHA256}" "slhdsa-c adaptation patch"
expect_hash "${HARNESS_FILE}" "${EXPECTED_HARNESS_SHA256}" "C verifier harness"
expect_hash "${DRIVER_FILE}" "${EXPECTED_DRIVER_SHA256}" "conformance driver"
expect_hash "${LEGACY_VECTOR}" "${EXPECTED_LEGACY_VECTOR_SHA256}" "legacy regression vector"
expect_hash "${CANONICAL_VECTOR}" "${EXPECTED_CANONICAL_VECTOR_SHA256}" "canonical IPD vector"

TASK_TMP_ROOT="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "${TASK_TMP_ROOT%/}/slh-dsa-conformance.XXXXXX")"
cleanup() {
  chmod -R u+w "${WORK_DIR}" 2>/dev/null || true
  rm -rf -- "${WORK_DIR}"
}
trap cleanup EXIT

# A local clone ensures the user-supplied source tree is never patched in place.
git clone --quiet --no-hardlinks "${SOURCE_PATH}" "${WORK_DIR}/slhdsa-c"
git -C "${WORK_DIR}/slhdsa-c" apply --check "${PATCH_FILE}"
git -C "${WORK_DIR}/slhdsa-c" apply "${PATCH_FILE}"

"${CC_BIN}" -O2 -std=c99 -Wall -Wextra -Werror -pedantic -DSLH_EXPERIMENTAL \
  -I"${WORK_DIR}/slhdsa-c" \
  "${HARNESS_FILE}" \
  "${WORK_DIR}/slhdsa-c/sha2_256.c" \
  "${WORK_DIR}/slhdsa-c/sha2_512.c" \
  "${WORK_DIR}/slhdsa-c/sha3_api.c" \
  "${WORK_DIR}/slhdsa-c/sha3_f1600.c" \
  "${WORK_DIR}/slhdsa-c/slh_dsa.c" \
  "${WORK_DIR}/slhdsa-c/slh_prehash.c" \
  "${WORK_DIR}/slhdsa-c/slh_sha2.c" \
  "${WORK_DIR}/slhdsa-c/slh_shake.c" \
  -o "${WORK_DIR}/verify-sp800-230-ipd"

"${PYTHON_BIN}" "${DRIVER_FILE}" \
  "${WORK_DIR}/verify-sp800-230-ipd" \
  "${LEGACY_VECTOR}" \
  "${CANONICAL_VECTOR}"
