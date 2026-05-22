#!/usr/bin/env bash
# create-tx-filterer + convert-to-gateway (§4).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_assert_zksync_era_sha
gl_path_for_zkstack
: "${GATEWAY_DIR:=${HOME}/gateway}"
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${NATIVE_TOKEN_PRICE_USD:=0.01}"
: "${GATEWAY_INTEROP_FEE_USD:=${INTEROP_FEE_USD:-0.15}}"
cd "${GATEWAY_DIR}"

if [ -z "${GATEWAY_SETTLEMENT_FEE:-}" ]; then
  export GATEWAY_SETTLEMENT_FEE="$(
    python3 - <<'PY'
import os
from decimal import Decimal, ROUND_CEILING, getcontext

getcontext().prec = 80

target_usd = Decimal(os.environ["GATEWAY_INTEROP_FEE_USD"])
native_price_usd = Decimal(os.environ["NATIVE_TOKEN_PRICE_USD"])
decimals = int(os.environ.get("GATEWAY_INTEROP_FEE_TOKEN_DECIMALS", "18"))

if target_usd < 0:
    raise SystemExit("GATEWAY_INTEROP_FEE_USD must be non-negative")
if native_price_usd <= 0:
    raise SystemExit("NATIVE_TOKEN_PRICE_USD must be positive")
if decimals < 0:
    raise SystemExit("GATEWAY_INTEROP_FEE_TOKEN_DECIMALS must be non-negative")

fee = (target_usd / native_price_usd * (Decimal(10) ** decimals)).to_integral_value(
    rounding=ROUND_CEILING
)
print(int(fee))
PY
  )"
fi
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

gl_zkstack_pty zkstack chain gateway create-tx-filterer --chain "${GATEWAY_CHAIN_NAME}"
gl_zkstack_pty zkstack chain gateway convert-to-gateway --chain "${GATEWAY_CHAIN_NAME}"
