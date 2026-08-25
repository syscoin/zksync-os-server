#!/usr/bin/env bash
# End-to-end test runner for the zkSYS gas-tank fee flow.
#
# Usage:
#   ZKSYNC_OS_PATH=/path/to/patched/zksync-os ./run.sh
#
# ZKSYNC_OS_PATH must point at a zksync-os checkout with the Syscoin patch
# SYSCOIN: applied with scripts/apply-zksync-os-syscoin-v0.4.0-patch.sh. The runner temporarily
# regenerates the checkout's syscoin_edge_da.rs with the test gas-tank address
# (0x3333...33) expected by tests/gas_tank.rs, and restores the original file
# afterwards. The Solidity twin of this suite lives in
# integration-tests/test-contracts/test/ZkSysGasTank.t.sol.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ZKSYNC_OS_PATH="${ZKSYNC_OS_PATH:-}"
if [[ -z "${ZKSYNC_OS_PATH}" ]]; then
  echo "error: set ZKSYNC_OS_PATH to a zksync-os checkout with the Syscoin patch applied" >&2
  exit 1
fi
ZKSYNC_OS_PATH="$(cd "${ZKSYNC_OS_PATH}" && pwd)"

GAS_TANK_SRC="${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_gas_tank.rs"
EDGE_DA_SRC="${ZKSYNC_OS_PATH}/basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_edge_da.rs"
if [[ ! -f "${GAS_TANK_SRC}" || ! -f "${EDGE_DA_SRC}" ]]; then
  echo "error: gas-tank patch not present in ${ZKSYNC_OS_PATH}; apply scripts/apply-zksync-os-syscoin-v0.4.0-patch.sh first" >&2
  exit 1
fi

# Bake the test tank address; restore the original generated file on exit.
TEST_TANK_BYTES="0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33"
BACKUP="$(mktemp)"
cp "${EDGE_DA_SRC}" "${BACKUP}"
restore_edge_da() {
  cp "${BACKUP}" "${EDGE_DA_SRC}"
  rm -f "${BACKUP}"
}
trap restore_edge_da EXIT

python3 - "${EDGE_DA_SRC}" "${TEST_TANK_BYTES}" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
tank_bytes = sys.argv[2]
text = path.read_text(encoding="utf-8")
pattern = re.compile(
    r"(pub fn syscoin_gas_tank_address\(\) -> B160 \{\n    B160::from_be_bytes\(\[)[^\]]*(\]\)\n\})"
)
new_text, count = pattern.subn(rf"\g<1>{tank_bytes}\g<2>", text)
if count != 1:
    raise SystemExit("failed to bake test gas tank address into syscoin_edge_da.rs")
path.write_text(new_text, encoding="utf-8")
PY

# Generate Cargo.toml with an absolute path dependency; a symlink inside the
# crate directory would break cargo's workspace-root discovery for the rig
# crate's `workspace = true` keys.
sed "s|@ZKSYNC_OS_PATH@|${ZKSYNC_OS_PATH}|g" "${SCRIPT_DIR}/Cargo.toml.template" \
  > "${SCRIPT_DIR}/Cargo.toml"

# Use the toolchain pinned by the zksync-os checkout.
TOOLCHAIN=""
if [[ -f "${ZKSYNC_OS_PATH}/rust-toolchain" ]]; then
  TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' "${ZKSYNC_OS_PATH}/rust-toolchain")"
elif [[ -f "${ZKSYNC_OS_PATH}/rust-toolchain.toml" ]]; then
  TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' "${ZKSYNC_OS_PATH}/rust-toolchain.toml")"
fi

cd "${SCRIPT_DIR}"
if [[ -n "${TOOLCHAIN}" ]]; then
  cargo "+${TOOLCHAIN}" test --release -- --nocapture
else
  cargo test --release -- --nocapture
fi
