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
: "${GATEWAY_MAX_L1_GAS_PRICE:=1000000000}"
: "${L2_BRIDGEHUB_ADDRESS:=0x0000000000000000000000000000000000010002}"

gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"
gl_ensure_chain_contracts_yaml_schema "${GATEWAY_CHAIN_NAME}"

ensure_gateway_rpc_url_in_chain_secrets() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc_url="${2:?gateway rpc url required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/configs/secrets.yaml" "${gateway_rpc_url}" <<'PY'
import sys
from pathlib import Path
import yaml

sys.set_int_max_str_digits(0)

secrets_path = Path(sys.argv[1])
gateway_rpc_url = sys.argv[2].strip()
if gateway_rpc_url == "":
    raise SystemExit("empty gateway rpc url")
if not secrets_path.exists():
    raise SystemExit(f"missing secrets config: {secrets_path}")

data = yaml.safe_load(secrets_path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {secrets_path}")

l1 = data.get("l1")
if l1 is None:
    l1 = {}
    data["l1"] = l1
if not isinstance(l1, dict):
    raise SystemExit(f"invalid l1 section in {secrets_path}")

current = l1.get("gateway_rpc_url")
if not isinstance(current, str) or current.strip() == "":
    l1["gateway_rpc_url"] = gateway_rpc_url
    secrets_path.write_text(
        yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )
    print(
        f"gateway-launch: patched {secrets_path} "
        f"(set l1.gateway_rpc_url={gateway_rpc_url})"
    )
PY
}

ensure_gateway_rpc_url_in_chain_secrets "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}"

get_chain_id_from_zkstack_yaml() {
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

sys.set_int_max_str_digits(0)

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

sys.set_int_max_str_digits(0)

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

get_chain_diamond_proxy_from_gateway() {
  local chain_name="${1:?chain name required}"
  local chain_id
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  cast call "${L2_BRIDGEHUB_ADDRESS}" "getZKChain(uint256)(address)" "${chain_id}" --rpc-url "${GATEWAY_RPC_URL}" | awk '{print $1}'
}

is_da_pair_set_on_gateway() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc="${2:?gateway rpc required}"
  local chain_proxy raw_pair line1 line2 raw_tokens
  chain_proxy="$(get_chain_diamond_proxy_from_gateway "${chain_name}")"

  if [ -z "${chain_proxy}" ] || [ "${chain_proxy}" = "0x0000000000000000000000000000000000000000" ]; then
    return 1
  fi

  if ! raw_pair="$(cast call "${chain_proxy}" "getDAValidatorPair()(address,uint8)" --rpc-url "${gateway_rpc}" 2>/dev/null)"; then
    if ! raw_pair="$(cast call "${chain_proxy}" "getDAValidatorPair()(address,address)" --rpc-url "${gateway_rpc}" 2>/dev/null)"; then
      return 1
    fi
  fi

  # cast output varies by version:
  # - multiline:
  #     0x...
  #     3
  # - single-line tuple:
  #     (0x..., 3)
  # Normalize to two tokens so detection is idempotent across cast versions.
  raw_tokens="$(printf '%s\n' "${raw_pair}" | tr '(),\n\t' '     ')"
  line1="$(printf '%s\n' "${raw_tokens}" | awk '{print $1}')"
  line2="$(printf '%s\n' "${raw_tokens}" | awk '{print $2}')"

  [ -n "${line1}" ] || return 1
  [ -n "${line2}" ] || return 1
  [ "${line1}" != "0x0000000000000000000000000000000000000000" ] || return 1

  # In Gateway mode, second value may be uint8 commitment scheme (e.g. 3).
  case "${line2}" in
  0 | 0x0 | 0x0000000000000000000000000000000000000000)
    return 1
    ;;
  esac
}

get_l1_da_validator_for_edge() {
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

sys.set_int_max_str_digits(0)

def norm(value):
    if value is None:
        return None
    if isinstance(value, int):
        return "0x" + format(value & ((1 << 160) - 1), "040x")
    if isinstance(value, str):
        value = value.strip()
        if value == "":
            return None
        return value
    return str(value)

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing contracts config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {p}")
l1 = data.get("l1")
if not isinstance(l1, dict):
    raise SystemExit(f"invalid l1 section in {p}")

candidates = [
    l1.get("blobs_zksync_os_l1_da_validator_addr"),
    l1.get("rollup_l1_da_validator_addr"),
]
for candidate in candidates:
    value = norm(candidate)
    if value is None:
        continue
    if value.lower() != "0x0000000000000000000000000000000000000000":
        print(value)
        raise SystemExit(0)

raise SystemExit(f"missing non-zero L1 DA validator in {p}")
PY
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

finalize_output=""
if ! finalize_output="$(gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  --deploy-paymaster false 2>&1)"; then
  echo "${finalize_output}"
  case "${finalize_output,,}" in
  *"depositdoesnotexist"*)
    echo "gateway-launch: finalize reported DepositDoesNotExist; treating as already-finalized deposit leg and continuing with DA repair"
    ;;
  *)
    exit 1
    ;;
  esac
else
  echo "${finalize_output}"
fi

if ! is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}"; then
  l1_da_validator_addr="$(get_l1_da_validator_for_edge "${EDGE_CHAIN_NAME}")"
  echo "gateway-launch: DA pair still missing on Gateway; setting it explicitly via zkstack chain set-da-validator-pair"
  gl_zkstack_pty zkstack chain set-da-validator-pair \
    --chain "${EDGE_CHAIN_NAME}" \
    --gateway \
    "${l1_da_validator_addr}" \
    BlobsAndPubdataKeccak256 \
    "${GATEWAY_MAX_L1_GAS_PRICE}"
fi

if ! is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}"; then
  echo "gateway-launch: DA validator pair is still not set on Gateway for ${EDGE_CHAIN_NAME} after repair attempt" >&2
  exit 1
fi
