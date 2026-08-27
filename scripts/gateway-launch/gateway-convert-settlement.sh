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
if fee < 0:
    raise SystemExit("GATEWAY_SETTLEMENT_FEE must be non-negative")

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
