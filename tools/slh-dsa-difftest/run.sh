#!/usr/bin/env bash
# Differential test runner for the SLH-DSA-SHA2-128-24 precompile.
#
# Usage:
#   ZKSYNC_OS_PATH=/path/to/patched/zksync-os ./run.sh
#
# ZKSYNC_OS_PATH must point at a zksync-os checkout with the Syscoin patch
# applied (scripts/apply-zksync-os-syscoin-patch.sh). The Solidity twin of this
# suite lives in contracts/test/SLHDSASHA212824Differential.t.sol and shares
# the same vector file and mutation scheme; run it with `forge test` from the
# contracts directory.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ZKSYNC_OS_PATH="${ZKSYNC_OS_PATH:-}"
if [[ -z "${ZKSYNC_OS_PATH}" ]]; then
  echo "error: set ZKSYNC_OS_PATH to a zksync-os checkout with the Syscoin patch applied" >&2
  exit 1
fi
ZKSYNC_OS_PATH="$(cd "${ZKSYNC_OS_PATH}" && pwd)"

SLH_DSA_SRC="${ZKSYNC_OS_PATH}/basic_system/src/system_functions/slh_dsa_sha2_128_24_verify.rs"
if [[ ! -f "${SLH_DSA_SRC}" ]]; then
  echo "error: ${SLH_DSA_SRC} not found; apply scripts/apply-zksync-os-syscoin-patch.sh first" >&2
  exit 1
fi

# Generate Cargo.toml with an absolute path dependency; a symlink inside the
# crate directory would break cargo's workspace-root discovery for
# basic_system's `workspace = true` keys.
sed "s|@ZKSYNC_OS_PATH@|${ZKSYNC_OS_PATH}|g" "${SCRIPT_DIR}/Cargo.toml.template" \
  > "${SCRIPT_DIR}/Cargo.toml"

# Use the toolchain pinned by the zksync-os checkout so basic_system builds
# with the same nightly features it expects.
TOOLCHAIN=""
if [[ -f "${ZKSYNC_OS_PATH}/rust-toolchain" ]]; then
  TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' "${ZKSYNC_OS_PATH}/rust-toolchain")"
elif [[ -f "${ZKSYNC_OS_PATH}/rust-toolchain.toml" ]]; then
  TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' "${ZKSYNC_OS_PATH}/rust-toolchain.toml")"
fi

cd "${SCRIPT_DIR}"
if [[ -n "${TOOLCHAIN}" ]]; then
  exec cargo "+${TOOLCHAIN}" test --release -- --nocapture
else
  exec cargo test --release -- --nocapture
fi
