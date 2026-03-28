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
: "${GATEWAY_RPC_URL:=http://127.0.0.1:3052}"

gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"

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

get_chain_diamond_proxy_from_contracts_yaml() {
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing contracts config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {p}")
l1 = data.get("l1")
if not isinstance(l1, dict):
    raise SystemExit(f"invalid l1 section in {p}")
diamond = l1.get("diamond_proxy_addr")
if diamond is None:
    raise SystemExit(f"missing l1.diamond_proxy_addr in {p}")
if isinstance(diamond, int):
    diamond = "0x" + format(diamond & ((1 << 160) - 1), "040x")
print(str(diamond))
PY
}

is_da_pair_set_on_gateway() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc="${2:?gateway rpc required}"
  local chain_proxy raw_pair line1 line2
  chain_proxy="$(get_chain_diamond_proxy_from_contracts_yaml "${chain_name}")"

  if ! cast call "${chain_proxy}" "getDAValidatorPair()(address,address)" --rpc-url "${gateway_rpc}" >/dev/null 2>&1; then
    return 1
  fi

  raw_pair="$(cast call "${chain_proxy}" "getDAValidatorPair()(address,address)" --rpc-url "${gateway_rpc}")"
  line1="$(printf '%s\n' "${raw_pair}" | awk 'NR==1 {print $1}')"
  line2="$(printf '%s\n' "${raw_pair}" | awk 'NR==2 {print $1}')"

  [ -n "${line1}" ] || return 1
  [ -n "${line2}" ] || return 1
  [ "${line1}" != "0x0000000000000000000000000000000000000000" ] || return 1
  [ "${line2}" != "0x0000000000000000000000000000000000000000" ] || return 1
}

gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ] && is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}"; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id} with DA pair set; skipping migrate+finalize"
  exit 0
fi

if [ "${current_settlement_layer}" != "${gateway_chain_id}" ]; then
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

  migrate_output=""
  if ! migrate_output="$(gl_zkstack_pty zkstack chain gateway migrate-to-gateway \
    --chain "${EDGE_CHAIN_NAME}" \
    --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
    -v 2>&1)"; then
    echo "${migrate_output}"
    case "${migrate_output,,}" in
    *"already on top of gateway"*)
      echo "gateway-launch: ${EDGE_CHAIN_NAME} is already on Gateway settlement; continuing to finalize/post-migration steps"
      ;;
    *)
      exit 1
      ;;
    esac
  else
    echo "${migrate_output}"
  fi
else
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway; running finalize/post-migration steps to restore missing state"
fi

gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  --deploy-paymaster false
