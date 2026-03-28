#!/usr/bin/env bash
# Migrate edge chain to Gateway settlement (§7).
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
: "${L1_RPC_URL:?L1_RPC_URL is required}"
cd "${GATEWAY_DIR}"

: "${EDGE_CHAIN_NAME:=zksys}"
: "${GATEWAY_CHAIN_NAME:=gateway}"

gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"

# Fast idempotency check: if edge already settles on Gateway, skip the whole stage.
edge_chain_id="$(python3 - "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/ZkStack.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing chain config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict) or data.get("chain_id") is None:
    raise SystemExit(f"missing chain_id in {p}")
print(int(data["chain_id"]))
PY
)"
gateway_chain_id_fast="$(python3 - "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/ZkStack.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing chain config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict) or data.get("chain_id") is None:
    raise SystemExit(f"missing chain_id in {p}")
print(int(data["chain_id"]))
PY
)"
bridgehub_addr_fast="$(python3 - "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing contracts config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {p}")
eco = data.get("ecosystem_contracts")
if not isinstance(eco, dict):
    raise SystemExit(f"invalid ecosystem_contracts section in {p}")
bridgehub = eco.get("bridgehub_proxy_addr")
if bridgehub is None:
    raise SystemExit(f"missing ecosystem_contracts.bridgehub_proxy_addr in {p}")
if isinstance(bridgehub, int):
    bridgehub = "0x" + format(bridgehub & ((1 << 160) - 1), "040x")
print(str(bridgehub))
PY
)"
current_settlement_layer_fast="$(cast call "${bridgehub_addr_fast}" "settlementLayer(uint256)(uint256)" "${edge_chain_id}" --rpc-url "${L1_RPC_URL}" | awk '{print $1}')"
if [ "${current_settlement_layer_fast}" = "${gateway_chain_id_fast}" ]; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id_fast}; skipping migrate stage"
  exit 0
fi

pause_output=""
if ! pause_output="$(gl_zkstack_pty zkstack chain pause-deposits --chain "${EDGE_CHAIN_NAME}" -v 2>&1)"; then
  echo "${pause_output}"
  case "${pause_output,,}" in
  *"already paused"* | *"already been paused"* | *"depositsalreadypaused"*)
    echo "gateway-launch: deposits are already paused for ${EDGE_CHAIN_NAME}; continuing migration"
    ;;
  *)
    exit 1
    ;;
  esac
else
  echo "${pause_output}"
fi

get_chain_id_from_zkstack_yaml() {
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing chain config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict) or data.get("chain_id") is None:
    raise SystemExit(f"missing chain_id in {p}")
print(int(data["chain_id"]))
PY
}

get_settlement_layer_chain_id() {
  local chain_name="${1:?chain name required}"
  local chain_id bridgehub
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  bridgehub="$(python3 - "${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing contracts config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {p}")
eco = data.get("ecosystem_contracts")
if not isinstance(eco, dict):
    raise SystemExit(f"invalid ecosystem_contracts section in {p}")
bridgehub = eco.get("bridgehub_proxy_addr")
if bridgehub is None:
    raise SystemExit(f"missing ecosystem_contracts.bridgehub_proxy_addr in {p}")
if isinstance(bridgehub, int):
    bridgehub = "0x" + format(bridgehub & ((1 << 160) - 1), "040x")
print(str(bridgehub))
PY
)"

  cast call "${bridgehub}" "settlementLayer(uint256)(uint256)" "${chain_id}" --rpc-url "${L1_RPC_URL}" | awk '{print $1}'
}

gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ]; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id}; skipping migrate+finalize"
  exit 0
fi

gl_zkstack_pty zkstack chain gateway migrate-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  -v

current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ]; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} settlement layer already switched to Gateway; skipping finalize rerun"
  exit 0
fi

gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  --deploy-paymaster false
