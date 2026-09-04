#!/usr/bin/env bash
# create-tx-filterer + convert-to-gateway (§4).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
# SYSCOIN: Convert settlement only for the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
# SYSCOIN: Normalize direct/resumed conversion to the same checkpoint-bound
# deployment EVM target selected by the canonical launcher (Cancun by default).
gl_export_foundry_evm_version
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${NATIVE_TOKEN_PRICE_USD:=0.01}"
: "${GATEWAY_INTEROP_FEE_USD:=${INTEROP_FEE_USD:-0.15}}"
export GATEWAY_DIR NATIVE_TOKEN_PRICE_USD GATEWAY_INTEROP_FEE_USD
cd "${GATEWAY_DIR}"

# SYSCOIN: Keep direct conversion on the reviewed Gateway/L1 pair; both
# zkstack subcommands below can broadcast irreversible settlement changes.
gl_validate_l1_network_pair
gl_normalize_canonical_deployment_inputs
gl_bind_gateway_launch_context
gl_assert_gateway_chain_config_matches_expected
gl_l1_broadcast_preflight
conversion_deployer="$(gl_authenticate_chain_wallet_roles --print-addresses "${GATEWAY_CHAIN_NAME}" deployer)"
# SYSCOIN: Persist the one transient whitelist principal before conversion.
# Wallet rotation must never hide an interrupted run's still-privileged deployer.
gl_bind_gateway_conversion_deployer "${conversion_deployer}"

# SYSCOIN: Canonical genesis freezes this relay from a Prague build, while the
# ordinary L1/Gateway artifacts remain on the checkpoint-bound deployment EVM
# target (Cancun by default). Build only the relay into a private tree
# and bind Forge to that exact artifact.
gl_assert_era_contracts_syscoin_postimage
SYSCOIN_EDGE_DA_RELAY_WORK_DIR=""
cleanup_syscoin_edge_da_relay_artifact() {
  local rc=$? cleanup_rc=0
  trap - EXIT HUP INT TERM
  if [ -n "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}" ]; then
    case "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}" in
      "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out/".syscoin-edge-da-relay.*)
        rm -rf -- "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR:?}" || cleanup_rc=$?
        ;;
      *)
        echo "gateway-launch: refusing to remove unexpected relay work directory: ${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}" >&2
        cleanup_rc=1
        ;;
    esac
  fi
  if [ "${rc}" -ne 0 ]; then
    exit "${rc}"
  fi
  exit "${cleanup_rc}"
}
trap cleanup_syscoin_edge_da_relay_artifact EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

SYSCOIN_EDGE_DA_RELAY_SCRIPT_OUT="${ZKSYNC_ERA_PATH}/contracts/l1-contracts/script-out"
mkdir -p -- "${SYSCOIN_EDGE_DA_RELAY_SCRIPT_OUT}"
[ -d "${SYSCOIN_EDGE_DA_RELAY_SCRIPT_OUT}" ] && [ ! -L "${SYSCOIN_EDGE_DA_RELAY_SCRIPT_OUT}" ] ||
  gl_die "relay script-out is missing, not a directory, or a symlink"
SYSCOIN_EDGE_DA_RELAY_WORK_DIR="$(
  umask 077
  mktemp -d "${SYSCOIN_EDGE_DA_RELAY_SCRIPT_OUT}/.syscoin-edge-da-relay.XXXXXX"
)"

cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
FOUNDRY_PROFILE=default FOUNDRY_EVM_VERSION=prague FOUNDRY_FORCE=true \
  forge build \
    contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol \
    --skip test \
    --force \
    --out "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}/out" \
    --cache-path "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}/cache"
SYSCOIN_EDGE_DA_RELAY_ARTIFACT="${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}/out/SyscoinRelayedSLDAValidator.sol/SyscoinRelayedSLDAValidator.json"
export SYSCOIN_EDGE_DA_RELAY_ARTIFACT

python3 - \
  "${SYSCOIN_EDGE_DA_RELAY_WORK_DIR}" \
  "${SYSCOIN_EDGE_DA_RELAY_ARTIFACT}" \
  "$(command -v cast)" <<'PY'
import json
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
artifact = Path(sys.argv[2])
cast = sys.argv[3]
if root.is_symlink() or not root.is_dir():
    raise SystemExit("invalid relay artifact work directory")
if artifact.is_symlink() or not artifact.is_file():
    raise SystemExit("relay artifact is missing, not regular, or a symlink")
resolved_root = root.resolve(strict=True)
try:
    artifact.resolve(strict=True).relative_to(resolved_root)
except ValueError:
    raise SystemExit("relay artifact escaped its private work directory") from None

payload = json.loads(artifact.read_text(encoding="utf-8"))
checks = (
    ("bytecode", 1618, "0x3b2e17477401a6d3df4356c346fdc18278330bbefca56154a81da87cbfd44bf2"),
    ("deployedBytecode", 1590, "0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0"),
)
for field, expected_size, expected_hash in checks:
    value = payload.get(field, {}).get("object")
    if not isinstance(value, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})+", value):
        raise SystemExit(f"invalid relay {field} object")
    if len(bytes.fromhex(value[2:])) != expected_size:
        raise SystemExit(f"unexpected relay {field} size")
    actual_hash = subprocess.check_output([cast, "keccak", value], text=True).strip().lower()
    if actual_hash != expected_hash:
        raise SystemExit(
            f"relay {field} hash mismatch: expected={expected_hash} actual={actual_hash}"
        )
PY
gl_assert_era_contracts_syscoin_postimage
cd "${GATEWAY_DIR}"

GATEWAY_SETTLEMENT_FEE="$(gl_effective_gateway_settlement_fee)" || exit $?
export GATEWAY_SETTLEMENT_FEE
echo "gateway-launch: Gateway interop settlement fee=${GATEWAY_SETTLEMENT_FEE} base units (target ${GATEWAY_INTEROP_FEE_USD} USD at native token ${NATIVE_TOKEN_PRICE_USD} USD)"

python3 - <<'PY'
import os
from pathlib import Path

import yaml

config_path = Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml"
if not config_path.exists():
    raise SystemExit(f"missing initial deployments config: {config_path}")

config = yaml.safe_load(config_path.read_text(encoding="utf-8"))
if not isinstance(config, dict):
    raise SystemExit(f"invalid initial deployments config: {config_path}")

fee_raw = os.environ["GATEWAY_SETTLEMENT_FEE"].strip()
fee = int(fee_raw, 16) if fee_raw.lower().startswith("0x") else int(fee_raw, 10)
# SYSCOIN: Fee-payer provisioning requires a live non-zero settlement fee;
# reject an inconsistent deployment before either Gateway broadcast.
if fee <= 0:
    raise SystemExit("GATEWAY_SETTLEMENT_FEE must be non-zero")

# zkstack accepts the historical hex string shape here; keep that style.
config["gateway_settlement_fee"] = hex(fee)
config_path.write_text(yaml.safe_dump(config, sort_keys=False), encoding="utf-8")
print(f"gateway-launch: wrote {config_path} gateway_settlement_fee={config['gateway_settlement_fee']}")
PY

gl_zkstack_pty zkstack chain gateway create-tx-filterer \
  --chain "${GATEWAY_CHAIN_NAME}" \
  --l1-rpc-url "${L1_RPC_URL}"
gl_zkstack_pty zkstack chain gateway convert-to-gateway \
  --chain "${GATEWAY_CHAIN_NAME}" \
  --l1-rpc-url "${L1_RPC_URL}"

gl_probe_gateway_settlement_ready ||
  gl_die "Gateway conversion completed but live settlement postconditions are not ready"
