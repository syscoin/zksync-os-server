#!/usr/bin/env bash
# Deploy only the zkSYS L1 registry bridge. This intentionally avoids the
# gateway ecosystem / L1 core deployment path in gateway-deploy-l1.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

BRIDGE_CHECK_ONLY=false
if [ "${1:-}" = "--check-only" ]; then
  BRIDGE_CHECK_ONLY=true
  shift
fi
[ "$#" -eq 0 ] || gl_die "usage: zksys-l1-registry-bridge-only.sh [--check-only]"
export ZKSYS_L1_REGISTRY_BRIDGE_CHECK_ONLY="${BRIDGE_CHECK_ONLY}"

gl_require GATEWAY_DIR
gl_require ZKSYNC_ERA_PATH
gl_require ZKSYNC_OS_SERVER_PATH
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
# SYSCOIN: Registry deployment targets the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_contracts_sha
gl_assert_zksync_era_sha
bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-era-contracts-syscoin-patch.sh" \
  "${ZKSYNC_ERA_PATH}/contracts"

gl_validate_l1_network_pair
gl_reject_no_proofs_on_mainnet
gl_validate_l1_signer_policy
gl_normalize_canonical_deployment_inputs
gl_export_foundry_evm_version
export FOUNDRY_CHAIN_ID="${L1_CHAIN_ID}"
gl_bind_gateway_launch_context
gl_assert_gateway_chain_config_matches_expected
gl_l1_broadcast_preflight

CREATE2_FACTORY_ADDR="$(
  python3 - <<'PY'
import os
from pathlib import Path

import yaml

d = yaml.safe_load((Path(os.environ["GATEWAY_DIR"]) / "configs" / "initial_deployments.yaml").read_text()) or {}
addr = d.get("create2_factory_addr", "0x4e59b44847b379578588920cA78FbF26c0B4956C")
if isinstance(addr, int):
    value = addr
else:
    raw = str(addr).strip()
    if raw.startswith(("0x", "0X")):
        value = int(raw[2:], 16)
    elif raw.isdecimal():
        value = int(raw, 10)
    else:
        value = int(raw, 16)
if value < 0 or value >= 1 << 160:
    raise SystemExit("create2_factory_addr must fit address")
print("0x" + format(value, "040x"))
PY
)"
if [ "${L1_NETWORK}" = "tanenbaum" ] || [ "${L1_NETWORK}" = "mainnet" ]; then
  [ "$(gl_to_lower "${CREATE2_FACTORY_ADDR}")" = "0x4e59b44847b379578588920ca78fbf26c0b4956c" ] ||
    gl_die "create2_factory_addr must be the canonical Arachnid factory"
fi

DEPLOYER_CAST_WALLET_ARGS=()
if [ "${BRIDGE_CHECK_ONLY}" != true ]; then
  case "$(gl_to_lower "${DEPLOYER_SIGNER:-account}")" in
account)
  gl_validate_foundry_account_keystore \
    "${DEPLOYER_ACCOUNT_NAME:?DEPLOYER_ACCOUNT_NAME required}" "DEPLOYER_ACCOUNT_NAME"
  DEPLOYER_CAST_WALLET_ARGS+=(--account "${DEPLOYER_ACCOUNT_NAME}")
  ;;
keystore)
  gl_require DEPLOYER_KEYSTORE
  gl_validate_secret_file "${DEPLOYER_KEYSTORE}" "DEPLOYER_KEYSTORE"
  DEPLOYER_CAST_WALLET_ARGS+=(--keystore "${DEPLOYER_KEYSTORE}")
  ;;
ledger)
  DEPLOYER_CAST_WALLET_ARGS+=(--ledger)
  ;;
trezor)
  DEPLOYER_CAST_WALLET_ARGS+=(--trezor)
  ;;
aws)
  DEPLOYER_CAST_WALLET_ARGS+=(--aws)
  ;;
gcp)
  DEPLOYER_CAST_WALLET_ARGS+=(--gcp)
  ;;
*)
  gl_die "unsupported DEPLOYER_SIGNER=${DEPLOYER_SIGNER:-} for bridge-only deployment"
  ;;
  esac

  if [ -n "${DEPLOYER_PASSWORD_FILE:-}" ]; then
    gl_validate_secret_file "${DEPLOYER_PASSWORD_FILE}" "DEPLOYER_PASSWORD_FILE"
    DEPLOYER_CAST_WALLET_ARGS+=(--password-file "${DEPLOYER_PASSWORD_FILE}")
  fi
  export DEPLOYER_ADDRESS="$(cast wallet address "${DEPLOYER_CAST_WALLET_ARGS[@]}")"
fi

helper_file="$(mktemp)"
cleanup() {
  rm -f "${helper_file}"
}
trap cleanup EXIT

python3 - "${SCRIPT_DIR}/gateway-deploy-l1.sh" "${helper_file}" <<'PY'
import sys
from pathlib import Path

source = Path(sys.argv[1]).read_text(encoding="utf-8")
start = source.index("cast_code_or_die() {")
end = source.index('\nif [ -n "${L1_WETH_TOKEN_ADDRESS}" ]')
Path(sys.argv[2]).write_text(source[start:end], encoding="utf-8")
PY

# shellcheck source=/dev/null
source "${helper_file}"
deploy_zksys_l1_registry_bridge
