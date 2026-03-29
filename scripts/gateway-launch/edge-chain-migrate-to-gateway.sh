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

gl_l1_broadcast_preflight

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
  local chain_id call_from
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"
  gateway_cast_call_with_fallback "${L2_BRIDGEHUB_ADDRESS}" "getZKChain(uint256)(address)" "${GATEWAY_RPC_URL}" "${call_from}" "${chain_id}" | awk '{print $1}'
}

get_chain_governor_from_wallets() {
  local chain_name="${1:?chain name required}"
  python3 - \
    "${GATEWAY_DIR}/chains/${chain_name}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/chains/${chain_name}/wallets.yaml" \
    "${GATEWAY_DIR}/configs/wallets.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

sys.set_int_max_str_digits(0)

for path_str in sys.argv[1:]:
    p = Path(path_str)
    if not p.exists():
        continue
    data = yaml.safe_load(p.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        continue
    gov = data.get("governor")
    if not isinstance(gov, dict):
        continue
    addr = gov.get("address")
    if isinstance(addr, int):
        addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
    if isinstance(addr, str) and addr.strip() != "":
        print(addr.strip())
        raise SystemExit(0)
raise SystemExit(0)
PY
}

gateway_cast_call_with_fallback() {
  local target="${1:?target required}"
  local sig="${2:?signature required}"
  local rpc_url="${3:?rpc url required}"
  local call_from="${4:-}"
  shift 4

  local out
  if [ -n "${call_from}" ]; then
    if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
      cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" --from "${call_from}" --gas-price 0 2>/dev/null)"; then
      printf '%s\n' "${out}"
      return 0
    fi
  fi
  if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" --gas-price 0 2>/dev/null)"; then
    printf '%s\n' "${out}"
    return 0
  fi
  if [ -n "${call_from}" ]; then
    if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
      cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" --from "${call_from}" 2>/dev/null)"; then
      printf '%s\n' "${out}"
      return 0
    fi
  fi
  if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" 2>/dev/null)"; then
    printf '%s\n' "${out}"
    return 0
  fi
  return 1
}

gateway_address_has_code() {
  local rpc_url="${1:?rpc url required}"
  local addr="${2:?address required}"

  local code
  if ! code="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast code "${addr}" --rpc-url "${rpc_url}" 2>/dev/null)"; then
    return 1
  fi
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ -n "${code}" ] || return 1
  [ "${code}" != "0x" ] || return 1
}

is_da_pair_set_on_gateway() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc="${2:?gateway rpc required}"
  local chain_proxy raw_pair line1 line2 raw_tokens call_from
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"
  chain_proxy="$(get_chain_diamond_proxy_from_gateway "${chain_name}")"

  if [ -z "${chain_proxy}" ] || [ "${chain_proxy}" = "0x0000000000000000000000000000000000000000" ]; then
    return 1
  fi

  if ! raw_pair="$(gateway_cast_call_with_fallback "${chain_proxy}" "getDAValidatorPair()(address,uint8)" "${gateway_rpc}" "${call_from}")"; then
    if ! raw_pair="$(gateway_cast_call_with_fallback "${chain_proxy}" "getDAValidatorPair()(address,address)" "${gateway_rpc}" "${call_from}")"; then
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
  gateway_address_has_code "${gateway_rpc}" "${line1}" || return 1

  # In Gateway mode, second value may be uint8 commitment scheme (e.g. 3).
  case "${line2}" in
  0 | 0x0 | 0x0000000000000000000000000000000000000000)
    return 1
    ;;
  esac
}

wait_for_da_pair_on_gateway() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc="${2:?gateway rpc required}"
  local attempts="${3:-6}"
  local delay_s="${4:-2}"
  local i

  for i in $(seq 1 "${attempts}"); do
    if is_da_pair_set_on_gateway "${chain_name}" "${gateway_rpc}"; then
      return 0
    fi
    if [ "${i}" -lt "${attempts}" ]; then
      sleep "${delay_s}"
    fi
  done
  return 1
}

get_l1_da_validator_for_edge() {
  local edge_chain_name="${1:?edge chain name required}"
  local gateway_chain_name="${2:?gateway chain name required}"
  local gateway_rpc_url="${3:?gateway rpc url required}"

  local raw_candidates
  raw_candidates="$(python3 - \
    "${EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR:-}" \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" \
    "${GATEWAY_DIR}/chains/${edge_chain_name}/configs/genesis.yaml" \
    "${GATEWAY_DIR}/chains/${edge_chain_name}/configs/contracts.yaml" <<'PY'
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
    value = str(value).strip()
    return value if value else None

def emit(value):
    value = norm(value)
    if value is None:
        return
    if value.lower() == "0x0000000000000000000000000000000000000000":
        return
    print(value)

# 1) Explicit override from env (highest precedence).
emit(sys.argv[1])

# 2) Canonical Gateway DA validator from gateway config.
#    This mirrors zkstack migration logic:
#    - Rollup chains use relayed_sl_da_validator
#    - Validium chains use validium_da_validator
gateway_cfg_path = Path(sys.argv[2])
genesis_cfg_path = Path(sys.argv[3])
commitment_mode = None
if genesis_cfg_path.exists():
    genesis_data = yaml.safe_load(genesis_cfg_path.read_text(encoding="utf-8"))
    if isinstance(genesis_data, dict):
        mode = genesis_data.get("l1_batch_commit_data_generator_mode")
        if isinstance(mode, str):
            commitment_mode = mode.strip().lower()

if gateway_cfg_path.exists():
    gateway_data = yaml.safe_load(gateway_cfg_path.read_text(encoding="utf-8"))
    if isinstance(gateway_data, dict):
        relayed = gateway_data.get("relayed_sl_da_validator")
        validium = gateway_data.get("validium_da_validator")
        # Rollup mode is the default when mode is absent.
        if commitment_mode == "validium":
            emit(validium)
            emit(relayed)
        else:
            emit(relayed)
            emit(validium)

# 3) Legacy L1 DA validator fields (fallback only).
contracts_path = Path(sys.argv[4])
if contracts_path.exists():
    data = yaml.safe_load(contracts_path.read_text(encoding="utf-8"))
    if isinstance(data, dict):
        l1 = data.get("l1")
        if isinstance(l1, dict):
            emit(l1.get("blobs_zksync_os_l1_da_validator_addr"))
            emit(l1.get("rollup_l1_da_validator_addr"))
PY
)"

  [ -n "${raw_candidates}" ] || {
    echo "missing DA validator candidates for ${edge_chain_name}" >&2
    return 1
  }

  local candidate
  while IFS= read -r candidate; do
    [ -n "${candidate}" ] || continue
    if gateway_address_has_code "${gateway_rpc_url}" "${candidate}"; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done <<EOF
${raw_candidates}
EOF

  echo "no DA validator candidate has bytecode on Gateway RPC (${gateway_rpc_url}) for ${edge_chain_name}; set EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR to a Gateway-deployed IL1DAValidator contract" >&2
  return 1
}

ensure_deposits_unpaused() {
  local chain_name="${1:?chain name required}"
  local unpause_output=""
  local unpause_output_lc=""

  gl_l1_broadcast_preflight
  if ! unpause_output="$(gl_zkstack_pty zkstack chain unpause-deposits --chain "${chain_name}" -v 2>&1)"; then
    echo "${unpause_output}"
    unpause_output_lc="$(gl_to_lower "${unpause_output}")"
    case "${unpause_output_lc}" in
    *"depositsnotpaused"* | *"already unpaused"* | *"deposits are not paused"* | *"not paused"*)
      echo "gateway-launch: deposits are already unpaused for ${chain_name}; continuing"
      ;;
    *)
      echo "gateway-launch: failed to unpause deposits for ${chain_name}" >&2
      return 1
      ;;
    esac
  else
    echo "${unpause_output}"
  fi
}

gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ] && is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}"; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id} with DA pair set; skipping migrate+finalize and ensuring deposits are unpaused"
  ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
  exit 0
fi

if [ "${current_settlement_layer}" != "${gateway_chain_id}" ]; then
  pause_output=""
  pause_output_lc=""
  if ! pause_output="$(gl_zkstack_pty zkstack chain pause-deposits --chain "${EDGE_CHAIN_NAME}" -v 2>&1)"; then
    echo "${pause_output}"
    pause_output_lc="$(gl_to_lower "${pause_output}")"
    case "${pause_output_lc}" in
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
  migrate_output_lc=""
  if ! migrate_output="$(gl_zkstack_pty zkstack chain gateway migrate-to-gateway \
    --chain "${EDGE_CHAIN_NAME}" \
    --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
    -v 2>&1)"; then
    echo "${migrate_output}"
    migrate_output_lc="$(gl_to_lower "${migrate_output}")"
    case "${migrate_output_lc}" in
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
finalize_output_lc=""
gl_l1_broadcast_preflight
if ! finalize_output="$(gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  --deploy-paymaster false 2>&1)"; then
  echo "${finalize_output}"
  finalize_output_lc="$(gl_to_lower "${finalize_output}")"
  case "${finalize_output_lc}" in
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

if ! wait_for_da_pair_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" 4 2; then
  l1_da_validator_addr="$(get_l1_da_validator_for_edge "${EDGE_CHAIN_NAME}" "${GATEWAY_CHAIN_NAME}" "${GATEWAY_RPC_URL}")"
  echo "gateway-launch: DA pair still missing on Gateway; setting it explicitly via zkstack chain set-da-validator-pair"
  gl_l1_broadcast_preflight
  gl_zkstack_pty zkstack chain set-da-validator-pair \
    --chain "${EDGE_CHAIN_NAME}" \
    --gateway \
    "${l1_da_validator_addr}" \
    BlobsAndPubdataKeccak256 \
    "${GATEWAY_MAX_L1_GAS_PRICE}"
fi

if ! wait_for_da_pair_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" 10 3; then
  echo "gateway-launch: DA validator pair is still not set on Gateway for ${EDGE_CHAIN_NAME} after repair attempt" >&2
  exit 1
fi

ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
