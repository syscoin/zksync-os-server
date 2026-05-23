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
: "${GATEWAY_L2_DA_COMMITMENT_SCHEME:=BlobsZKsyncOS}"
: "${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE:=4}"

# Gateway settlement checks must use the system Bridgehub on the Gateway chain.
# Do not allow accidental carry-over from shell/session env to point this at L1.
readonly GATEWAY_SYSTEM_BRIDGEHUB_ADDRESS="0x0000000000000000000000000000000000010002"
if [ -n "${L2_BRIDGEHUB_ADDRESS:-}" ]; then
  provided_l2_bridgehub="$(printf '%s' "${L2_BRIDGEHUB_ADDRESS}" | tr '[:upper:]' '[:lower:]')"
  expected_l2_bridgehub="$(printf '%s' "${GATEWAY_SYSTEM_BRIDGEHUB_ADDRESS}" | tr '[:upper:]' '[:lower:]')"
  if [ "${provided_l2_bridgehub}" != "${expected_l2_bridgehub}" ]; then
    echo "gateway-launch: invalid L2_BRIDGEHUB_ADDRESS override (${L2_BRIDGEHUB_ADDRESS}); expected ${GATEWAY_SYSTEM_BRIDGEHUB_ADDRESS}" >&2
    exit 1
  fi
fi
readonly L2_BRIDGEHUB_ADDRESS="${GATEWAY_SYSTEM_BRIDGEHUB_ADDRESS}"

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
        "(set l1.gateway_rpc_url=<redacted>)"
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

get_chain_diamond_proxy_from_gateway() {
  local chain_name="${1:?chain name required}"
  local chain_id call_from raw_proxy chain_proxy
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"
  if ! raw_proxy="$(gateway_cast_call_with_fallback "${L2_BRIDGEHUB_ADDRESS}" "getZKChain(uint256)(address)" "${GATEWAY_RPC_URL}" "${call_from}" "${chain_id}")"; then
    echo "gateway-launch: failed to query Gateway Bridgehub getZKChain(${chain_id}) for ${chain_name}; target=${L2_BRIDGEHUB_ADDRESS}, rpc=${GATEWAY_RPC_URL}, from=${call_from:-unset}, cast=$(command -v cast || true)" >&2
    return 1
  fi
  chain_proxy="$(printf '%s\n' "${raw_proxy}" | awk '{print $1}')"
  if [ -z "${chain_proxy}" ] || [ "${chain_proxy}" = "0x0000000000000000000000000000000000000000" ]; then
    echo "gateway-launch: Gateway Bridgehub returned empty chain proxy for ${chain_name} chain_id=${chain_id}: ${raw_proxy}" >&2
    return 1
  fi
  printf '%s\n' "${chain_proxy}"
}

wait_for_chain_diamond_proxy_from_gateway() {
  local chain_name="${1:?chain name required}"
  local attempts="${2:-30}"
  local delay="${3:-2}"
  local i chain_proxy

  for i in $(seq 1 "${attempts}"); do
    if chain_proxy="$(get_chain_diamond_proxy_from_gateway "${chain_name}")"; then
      printf '%s\n' "${chain_proxy}"
      return 0
    fi
    echo "gateway-launch: Gateway chain proxy for ${chain_name} not queryable yet (${i}/${attempts}); retrying in ${delay}s" >&2
    sleep "${delay}"
  done

  echo "gateway-launch: Gateway chain proxy for ${chain_name} did not become queryable after ${attempts} attempts" >&2
  return 1
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

get_chain_governor_private_key_from_wallets() {
  local chain_name="${1:?chain name required}"
  python3 - \
    "${GATEWAY_DIR}/chains/${chain_name}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/chains/${chain_name}/wallets.yaml" \
    "${GATEWAY_DIR}/configs/wallets.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

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
    private_key = gov.get("private_key")
    if isinstance(private_key, int):
        private_key = "0x" + format(private_key & ((1 << 256) - 1), "064x")
    if isinstance(private_key, str) and private_key.strip() != "":
        private_key = private_key.strip()
        if private_key.startswith(("0x", "0X")):
            private_key = "0x" + private_key[2:].zfill(64)
        print(private_key)
        raise SystemExit(0)
raise SystemExit("missing governor private_key in chain wallets")
PY
}

get_wallet_address_from_wallets() {
  local chain_name="${1:?chain name required}"
  local wallet_name="${2:?wallet name required}"
  python3 - \
    "${wallet_name}" \
    "${GATEWAY_DIR}/chains/${chain_name}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/chains/${chain_name}/wallets.yaml" \
    "${GATEWAY_DIR}/configs/wallets.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

wallet_name = sys.argv[1]
for path_str in sys.argv[2:]:
    p = Path(path_str)
    if not p.exists():
        continue
    data = yaml.safe_load(p.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        continue
    wallet = data.get(wallet_name)
    if not isinstance(wallet, dict):
        continue
    addr = wallet.get("address")
    if isinstance(addr, int):
        addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
    if isinstance(addr, str) and addr.strip() != "":
        print(addr.strip())
        raise SystemExit(0)
raise SystemExit(f"missing wallet address for {wallet_name}")
PY
}

GATEWAY_GOVERNOR_FORGE_WALLET_ARGS=()
GATEWAY_GOVERNOR_TEMP_DIR=""

cleanup_generated_gateway_governor_keystore() {
  if [ -n "${GATEWAY_GOVERNOR_TEMP_DIR:-}" ]; then
    rm -rf "${GATEWAY_GOVERNOR_TEMP_DIR}"
    GATEWAY_GOVERNOR_TEMP_DIR=""
  fi
}
trap cleanup_generated_gateway_governor_keystore EXIT

prepare_generated_gateway_governor_keystore() {
  local chain_name="${1:?chain name required}"
  local password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
  local account_name="gateway-launch-generated-governor"
  local expected_addr imported_addr

  [ -n "${password_file}" ] || {
    echo "gateway-launch: FUNDER_PASSWORD_FILE is required to encrypt the temporary generated-governor keystore" >&2
    return 1
  }
  [ -f "${password_file}" ] || {
    echo "gateway-launch: generated-governor password file does not exist: ${password_file}" >&2
    return 1
  }
  command -v expect >/dev/null 2>&1 || {
    echo "gateway-launch: expect is required to import the generated governor key without exposing it in argv" >&2
    return 1
  }

  if [ -z "${GATEWAY_GOVERNOR_TEMP_DIR}" ]; then
    GATEWAY_GOVERNOR_TEMP_DIR="$(mktemp -d)"
    chmod 700 "${GATEWAY_GOVERNOR_TEMP_DIR}"
    install -m 600 "${password_file}" "${GATEWAY_GOVERNOR_TEMP_DIR}/password"

    GATEWAY_DIR="${GATEWAY_DIR}" \
      CHAIN_NAME="${chain_name}" \
      KEYSTORE_DIR="${GATEWAY_GOVERNOR_TEMP_DIR}" \
      KEYSTORE_PASSWORD_FILE="${GATEWAY_GOVERNOR_TEMP_DIR}/password" \
      CAST_BIN="$(command -v cast)" \
      ACCOUNT_NAME="${account_name}" \
      expect <<'EXPECT'
set timeout 60
log_user 0
set pk [exec bash -c {python3 - "$GATEWAY_DIR" "$CHAIN_NAME" <<'PY'
import sys
from pathlib import Path
import yaml

gateway_dir = Path(sys.argv[1])
chain_name = sys.argv[2]
for p in [
    gateway_dir / "chains" / chain_name / "configs" / "wallets.yaml",
    gateway_dir / "chains" / chain_name / "wallets.yaml",
    gateway_dir / "configs" / "wallets.yaml",
]:
    if not p.exists():
        continue
    data = yaml.safe_load(p.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        continue
    gov = data.get("governor")
    if not isinstance(gov, dict):
        continue
    pk = gov.get("private_key")
    if isinstance(pk, int):
        print("0x" + format(pk & ((1 << 256) - 1), "064x"))
        raise SystemExit(0)
    if isinstance(pk, str) and pk.strip():
        s = pk.strip()
        print("0x" + s[2:].zfill(64) if s.startswith(("0x", "0X")) else s)
        raise SystemExit(0)
raise SystemExit("missing governor private_key")
PY
}]
set pw [exec sh -c {tr -d '\n' < "$KEYSTORE_PASSWORD_FILE"}]
spawn $env(CAST_BIN) wallet import $env(ACCOUNT_NAME) --keystore-dir $env(KEYSTORE_DIR) --interactive
expect -re "(?i).*private key.*"
send -- "$pk\r"
expect {
  -re "(?i).*password.*" {
    send -- "$pw\r"
    expect {
      -re "(?i).*(confirm|repeat|re-enter).*password.*" {
        send -- "$pw\r"
        expect eof
      }
      eof {}
    }
  }
  eof {}
}
EXPECT
  fi

  expected_addr="$(get_chain_governor_from_wallets "${chain_name}" | tr '[:upper:]' '[:lower:]')"
  imported_addr="$(cast wallet address --keystore "${GATEWAY_GOVERNOR_TEMP_DIR}/${account_name}" --password-file "${GATEWAY_GOVERNOR_TEMP_DIR}/password" | tr '[:upper:]' '[:lower:]')"
  if [ "${expected_addr}" != "${imported_addr}" ]; then
    echo "gateway-launch: generated-governor keystore mismatch: expected ${expected_addr}, got ${imported_addr}" >&2
    return 1
  fi

  GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--keystore "${GATEWAY_GOVERNOR_TEMP_DIR}/${account_name}" --password-file "${GATEWAY_GOVERNOR_TEMP_DIR}/password")
}

prepare_gateway_governor_forge_wallet_args() {
  GATEWAY_GOVERNOR_FORGE_WALLET_ARGS=()
  local governor_signer

  if [ -n "${EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY:-}" ]; then
    echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY is intentionally unsupported; use a Foundry keystore account, keystore file, hardware wallet, or KMS signer" >&2
    return 1
  fi

  if [ -n "${EDGE_GATEWAY_GOVERNOR_SIGNER:-}" ]; then
    governor_signer="${EDGE_GATEWAY_GOVERNOR_SIGNER}"
  else
    governor_signer="generated"
  fi
  governor_signer="$(gl_to_lower "${governor_signer}")"

  case "${governor_signer}" in
  generated | generated-wallet | wallet)
    prepare_generated_gateway_governor_keystore "${EDGE_CHAIN_NAME}"
    return
    ;;
  account)
    local account_name="${EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME:-${FUNDER_ACCOUNT_NAME:-funder}}"
    [ -n "${account_name}" ] || {
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME must not be empty" >&2
      return 1
    }
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--account "${account_name}")
    ;;
  keystore)
    local keystore_path="${EDGE_GATEWAY_GOVERNOR_KEYSTORE:-${FUNDER_KEYSTORE:-}}"
    [ -n "${keystore_path}" ] || {
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_KEYSTORE is required when EDGE_GATEWAY_GOVERNOR_SIGNER=keystore" >&2
      return 1
    }
    [ -f "${keystore_path}" ] || {
      echo "gateway-launch: governor keystore does not exist: ${keystore_path}" >&2
      return 1
    }
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--keystore "${keystore_path}")
    ;;
  ledger)
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--ledger)
    ;;
  trezor)
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--trezor)
    ;;
  aws)
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--aws)
    ;;
  gcp)
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--gcp)
    ;;
  private-key)
    if gl_l1_network_requires_external_signer && ! gl_allow_insecure_private_key_argv; then
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_SIGNER=private-key is not allowed on ${L1_NETWORK}; use a Foundry account/keystore, hardware wallet, or KMS signer" >&2
      return 1
    fi
    local governor_private_key="${FUNDER_PRIVATE_KEY:-}"
    if [ -z "${governor_private_key}" ]; then
      if gl_l1_network_requires_external_signer; then
        echo "gateway-launch: FUNDER_PRIVATE_KEY is required when inheriting EDGE_GATEWAY_GOVERNOR_SIGNER=private-key from FUNDER_SIGNER=private-key" >&2
        return 1
      fi
      governor_private_key="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    fi
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--private-key "${governor_private_key}")
    ;;
  *)
    echo "gateway-launch: unsupported EDGE_GATEWAY_GOVERNOR_SIGNER=${governor_signer}; expected generated, account, keystore, ledger, trezor, aws, gcp, or private-key" >&2
    return 1
    ;;
  esac

  local password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
  if [ -n "${password_file}" ]; then
    [ -f "${password_file}" ] || {
      echo "gateway-launch: governor password file does not exist: ${password_file}" >&2
      return 1
    }
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--password-file "${password_file}")
  fi
}

get_l1_bridgehub_proxy_addr() {
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
data = yaml.safe_load(p.read_text(encoding="utf-8"))
eco = data.get("ecosystem_contracts") if isinstance(data, dict) else None
if not isinstance(eco, dict):
    raise SystemExit(f"invalid ecosystem_contracts section in {p}")
addr = eco.get("bridgehub_proxy_addr")
if isinstance(addr, int):
    addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
if not isinstance(addr, str) or addr.strip() == "":
    raise SystemExit(f"missing ecosystem_contracts.bridgehub_proxy_addr in {p}")
print(addr.strip())
PY
}

get_gateway_validator_timelock_addr() {
  local gateway_chain_name="${1:?gateway chain name required}"
  python3 - "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" <<'PY'
import sys
from pathlib import Path
import yaml

p = Path(sys.argv[1])
if not p.exists():
    raise SystemExit(f"missing Gateway config: {p}")
data = yaml.safe_load(p.read_text(encoding="utf-8"))
addr = data.get("validator_timelock_addr") if isinstance(data, dict) else None
if isinstance(addr, int):
    addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
if not isinstance(addr, str) or addr.strip() == "":
    raise SystemExit(f"missing validator_timelock_addr in {p}")
print(addr.strip())
PY
}

gateway_cast_call_with_fallback() {
  local target="${1:?target required}"
  local sig="${2:?signature required}"
  local rpc_url="${3:?rpc url required}"
  local call_from="${4:-}"
  shift 4

  local out last_error=""
  if [ -n "${call_from}" ]; then
    # SYSCOIN: read-only Gateway calls must not inherit L1 broadcast fee env.
    # Gateway can have a different base fee, and cast applies ETH_GAS_PRICE to
    # eth_call transactions even though no transaction is broadcast.
    if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
      -u ETH_GAS_PRICE -u ETH_PRIORITY_GAS_PRICE -u ETH_MAX_FEE_PER_GAS -u ETH_MAX_PRIORITY_FEE_PER_GAS \
      cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" --from "${call_from}" 2>&1)"; then
      printf '%s\n' "${out}"
      return 0
    fi
    last_error="${out}"
  fi
  if out="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    -u ETH_GAS_PRICE -u ETH_PRIORITY_GAS_PRICE -u ETH_MAX_FEE_PER_GAS -u ETH_MAX_PRIORITY_FEE_PER_GAS \
    cast call "${target}" "${sig}" "$@" --rpc-url "${rpc_url}" 2>&1)"; then
    printf '%s\n' "${out}"
    return 0
  fi
  last_error="${out}"
  echo "gateway-launch: cast call failed: target=${target}, sig=${sig}, rpc=${rpc_url}, from=${call_from:-unset}, args=$*, last_error=${last_error}" >&2
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

gateway_committer_role_set() {
  local chain_name="${1:?chain name required}"
  local committer_addr="${2:?committer address required}"
  local chain_id validator_timelock committer_role call_from result
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  validator_timelock="$(get_gateway_validator_timelock_addr "${GATEWAY_CHAIN_NAME}")"
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"

  committer_role="$(gateway_cast_call_with_fallback \
    "${validator_timelock}" \
    "COMMITTER_ROLE()(bytes32)" \
    "${GATEWAY_RPC_URL}" \
    "${call_from}" | awk '{print $1}')"
  [ -n "${committer_role}" ] || return 1

  result="$(gateway_cast_call_with_fallback \
    "${validator_timelock}" \
    "hasRoleForChainId(uint256,bytes32,address)(bool)" \
    "${GATEWAY_RPC_URL}" \
    "${call_from}" \
    "${chain_id}" \
    "${committer_role}" \
    "${committer_addr}" | awk '{print $1}')" || return 1
  [ "${result}" = "true" ]
}

wait_for_gateway_committer_role() {
  local chain_name="${1:?chain name required}"
  local committer_addr="${2:?committer address required}"
  local max_attempts="${3:?max attempts required}"
  local delay_seconds="${4:?delay seconds required}"
  local attempt

  for attempt in $(seq 1 "${max_attempts}"); do
    if gateway_committer_role_set "${chain_name}" "${committer_addr}"; then
      return 0
    fi
    sleep "${delay_seconds}"
  done
  return 1
}

gateway_commit_sender_balance_wei() {
  local committer_addr="${1:?committer address required}"

  cast balance "${committer_addr}" --rpc-url "${GATEWAY_RPC_URL}"
}

gateway_commit_sender_funded() {
  local committer_addr="${1:?committer address required}"
  local min_balance_wei="${2:?minimum balance required}"
  local current_balance

  current_balance="$(gateway_commit_sender_balance_wei "${committer_addr}")" || return 1
  python3 - "${current_balance}" "${min_balance_wei}" <<'PY'
import sys

raise SystemExit(0 if int(sys.argv[1], 10) >= int(sys.argv[2], 10) else 1)
PY
}

wait_for_gateway_commit_sender_balance() {
  local committer_addr="${1:?committer address required}"
  local min_balance_wei="${2:?minimum balance required}"
  local max_attempts="${3:?max attempts required}"
  local delay_seconds="${4:?delay seconds required}"
  local attempt

  for attempt in $(seq 1 "${max_attempts}"); do
    if gateway_commit_sender_funded "${committer_addr}" "${min_balance_wei}"; then
      return 0
    fi
    sleep "${delay_seconds}"
  done
  return 1
}

ensure_gateway_commit_sender_validator() {
  local chain_name="${1:?chain name required}"
  local wallet_name committer_addr bridgehub validator_timelock refund_recipient gateway_chain_id chain_proxy committer_role grant_calldata

  # zkstack migration currently enables only `operator`, while OS-server commits
  # Syscoin DA batches from `blob_operator`. Keep this in the launch layer until
  # upstream migration accepts the actual commit sender set.
  wallet_name="${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}"
  committer_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")"

  if gateway_committer_role_set "${chain_name}" "${committer_addr}"; then
    echo "gateway-launch: Gateway committer role already set for ${wallet_name} (${committer_addr})"
    return 0
  fi

  echo "gateway-launch: Gateway committer role missing for ${wallet_name} (${committer_addr}); granting COMMITTER_ROLE via L1->Gateway admin tx"

  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")"
  validator_timelock="$(get_gateway_validator_timelock_addr "${GATEWAY_CHAIN_NAME}")"
  refund_recipient="$(get_chain_governor_from_wallets "${chain_name}")"
  gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
  chain_proxy="$(wait_for_chain_diamond_proxy_from_gateway "${chain_name}" 60 2)"
  committer_role="$(gateway_cast_call_with_fallback \
    "${validator_timelock}" \
    "COMMITTER_ROLE()(bytes32)" \
    "${GATEWAY_RPC_URL}" \
    "${refund_recipient}" | awk '{print $1}')"
  grant_calldata="$(cast calldata "grantRole(address,bytes32,address)" "${chain_proxy}" "${committer_role}" "${committer_addr}")"

  gl_l1_broadcast_preflight
  prepare_gateway_governor_forge_wallet_args
  (
    cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
    forge script deploy-scripts/AdminFunctions.s.sol:AdminFunctions \
      --sig 'adminL1L2Tx(address,uint256,uint256,address,uint256,bytes,address,bool)' \
      "${bridgehub}" \
      "${GATEWAY_MAX_L1_GAS_PRICE}" \
      "${gateway_chain_id}" \
      "${validator_timelock}" \
      0 \
      "${grant_calldata}" \
      "${refund_recipient}" \
      true \
      --rpc-url "${L1_RPC_URL}" \
      --broadcast \
      "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}" \
      --slow
  )

  : "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS:=120}"
  : "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY:=5}"
  echo "gateway-launch: waiting for Gateway committer role repair (up to $((GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS * GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY))s)"
  if ! wait_for_gateway_committer_role \
    "${chain_name}" \
    "${committer_addr}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY}"; then
    echo "gateway-launch: Gateway committer role still missing for ${wallet_name} (${committer_addr}) after repair attempt" >&2
    return 1
  fi
}

ensure_gateway_commit_sender_balance() {
  local chain_name="${1:?chain name required}"
  local bridgehub refund_recipient gateway_chain_id min_balance_wei
  local wallet_name sender_addr current_balance_wei top_up_wei

  # SYSCOIN: zksys settles on Gateway, so its Gateway L1-sender wallets need
  # Gateway base token for commit/prove/execute transactions. This is
  # funding-only; role grants are handled separately above.
  min_balance_wei="${GATEWAY_SENDER_MIN_BALANCE_WEI:-${GATEWAY_COMMITTER_MIN_BALANCE_WEI:-100000000000000000000}}"
  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")"
  refund_recipient="$(get_chain_governor_from_wallets "${chain_name}")"
  gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"

  for wallet_name in "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" prove_operator execute_operator; do
    sender_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")"
    current_balance_wei="$(gateway_commit_sender_balance_wei "${sender_addr}")"

    if python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

raise SystemExit(0 if int(sys.argv[1], 10) >= int(sys.argv[2], 10) else 1)
PY
    then
      echo "gateway-launch: Gateway sender balance already funded for ${wallet_name} (${sender_addr}): ${current_balance_wei} wei"
      continue
    fi

    top_up_wei="$(python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

print(int(sys.argv[2], 10) - int(sys.argv[1], 10))
PY
)"

    echo "gateway-launch: Gateway sender balance below minimum for ${wallet_name} (${sender_addr}): current=${current_balance_wei} wei, minimum=${min_balance_wei} wei; funding ${top_up_wei} wei via L1->Gateway admin tx"

    gl_l1_broadcast_preflight
    prepare_gateway_governor_forge_wallet_args
    (
      cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
      forge script deploy-scripts/AdminFunctions.s.sol:AdminFunctions \
        --sig 'adminL1L2Tx(address,uint256,uint256,address,uint256,bytes,address,bool)' \
        "${bridgehub}" \
        "${GATEWAY_MAX_L1_GAS_PRICE}" \
        "${gateway_chain_id}" \
        "${sender_addr}" \
        "${top_up_wei}" \
        "0x" \
        "${refund_recipient}" \
        true \
        --rpc-url "${L1_RPC_URL}" \
        --broadcast \
        "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}" \
        --slow
    )

    : "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS:=120}"
    : "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY:=5}"
    echo "gateway-launch: waiting for Gateway sender balance repair for ${wallet_name} (up to $((GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS * GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY))s)"
    if ! wait_for_gateway_commit_sender_balance \
      "${sender_addr}" \
      "${min_balance_wei}" \
      "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS}" \
      "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY}"; then
      echo "gateway-launch: Gateway sender balance still below ${min_balance_wei} wei for ${wallet_name} (${sender_addr}) after repair attempt" >&2
      return 1
    fi
  done
}

repair_da_pair_on_gateway() {
  local chain_name="${1:?chain name required}"
  local l1_da_validator_addr="${2:?L1 DA validator address required}"
  local bridgehub refund_recipient chain_id gateway_chain_id chain_proxy

  echo "gateway-launch: resolving Gateway DA pair repair inputs for ${chain_name}"
  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")" || return 1
  refund_recipient="$(get_chain_governor_from_wallets "${chain_name}")" || return 1
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")" || return 1
  gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")" || return 1
  # SYSCOIN: after zkstack finalize exits with an already-finalized deposit leg,
  # Gateway RPC can be live before Bridgehub getZKChain is queryable.
  chain_proxy="$(wait_for_chain_diamond_proxy_from_gateway "${chain_name}" 60 2)" || return 1

  echo "gateway-launch: repairing Gateway DA pair for ${chain_name}: l1_da_validator=${l1_da_validator_addr}, scheme=${GATEWAY_L2_DA_COMMITMENT_SCHEME}(${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE}), chain_proxy=${chain_proxy}, gateway_chain_id=${gateway_chain_id}"
  gl_l1_broadcast_preflight
  prepare_gateway_governor_forge_wallet_args
  (
    cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts"
    echo "gateway-launch: broadcasting setDAValidatorPairWithGateway repair via $(pwd)"
    forge script deploy-scripts/AdminFunctions.s.sol:AdminFunctions \
      --sig 'setDAValidatorPairWithGateway(address,uint256,uint256,uint256,address,uint8,address,address,bool)' \
      "${bridgehub}" \
      "${GATEWAY_MAX_L1_GAS_PRICE}" \
      "${chain_id}" \
      "${gateway_chain_id}" \
      "${l1_da_validator_addr}" \
      "${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE}" \
      "${chain_proxy}" \
      "${refund_recipient}" \
      true \
      --rpc-url "${L1_RPC_URL}" \
      --broadcast \
      "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}" \
      --slow
  )
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

  # The scheme must match the batches produced by OS-server. A non-zero validator
  # address with the wrong scheme still lets migration finish, but the first
  # commit reverts with MismatchL2DACommitmentScheme.
  [ "${line2}" = "${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE}" ] || return 1
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

  echo "no DA validator candidate has bytecode on the configured Gateway RPC for ${edge_chain_name}; set EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR to a Gateway-deployed IL1DAValidator contract" >&2
  return 1
}

ensure_deposits_unpaused() {
  local chain_name="${1:?chain name required}"
  local unpause_output=""
  local unpause_output_lc=""

  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${chain_name}"
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

refresh_l1_admin_wallet_funding() {
  local chain_name="${1:?chain name required}"

  # SYSCOIN: zkstack prompts interactively if the governor has less than 5 ETH,
  # even when the transaction would succeed. Migration can spend down wallets
  # after the earlier funding checkpoint, so refresh the chain wallet targets
  # immediately before zkstack admin broadcasts.
  GATEWAY_CHAIN_NAME="${chain_name}" "${SCRIPT_DIR}/fund-wallets.sh"
}

gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
edge_committer_wallet_name="${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}"
edge_committer_addr="$(get_wallet_address_from_wallets "${EDGE_CHAIN_NAME}" "${edge_committer_wallet_name}")"
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ] &&
  is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" &&
  gateway_committer_role_set "${EDGE_CHAIN_NAME}" "${edge_committer_addr}"; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id} with DA pair and committer role set; ensuring committer balance and deposits are unpaused"
  ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
  ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
  exit 0
fi

if [ "${current_settlement_layer}" != "${gateway_chain_id}" ]; then
  pause_output=""
  pause_output_lc=""
  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
  if ! pause_output="$(gl_zkstack_pty zkstack chain pause-deposits --chain "${EDGE_CHAIN_NAME}" -v 2>&1)"; then
    echo "${pause_output}"
    pause_output_lc="$(gl_to_lower "${pause_output}")"
    case "${pause_output_lc}" in
    *"already paused"* | *"already been paused"* | *"alreadypaused"* | *"depositsalreadypaused"*)
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
  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
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
refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
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

: "${GATEWAY_DA_PAIR_INITIAL_WAIT_ATTEMPTS:=4}"
: "${GATEWAY_DA_PAIR_INITIAL_WAIT_DELAY:=2}"
: "${GATEWAY_DA_PAIR_REPAIR_WAIT_ATTEMPTS:=120}"
: "${GATEWAY_DA_PAIR_REPAIR_WAIT_DELAY:=5}"

da_pair_repair_requested=false
if ! wait_for_da_pair_on_gateway \
  "${EDGE_CHAIN_NAME}" \
  "${GATEWAY_RPC_URL}" \
  "${GATEWAY_DA_PAIR_INITIAL_WAIT_ATTEMPTS}" \
  "${GATEWAY_DA_PAIR_INITIAL_WAIT_DELAY}"; then
  l1_da_validator_addr="$(get_l1_da_validator_for_edge "${EDGE_CHAIN_NAME}" "${GATEWAY_CHAIN_NAME}" "${GATEWAY_RPC_URL}")"
  echo "gateway-launch: DA pair still missing or has wrong scheme on Gateway; setting ${GATEWAY_L2_DA_COMMITMENT_SCHEME} explicitly"
  repair_da_pair_on_gateway "${EDGE_CHAIN_NAME}" "${l1_da_validator_addr}"
  da_pair_repair_requested=true
fi

if [ "${da_pair_repair_requested}" = true ]; then
  echo "gateway-launch: waiting for Gateway to apply the repaired DA pair (up to $((GATEWAY_DA_PAIR_REPAIR_WAIT_ATTEMPTS * GATEWAY_DA_PAIR_REPAIR_WAIT_DELAY))s)"
fi

if ! wait_for_da_pair_on_gateway \
  "${EDGE_CHAIN_NAME}" \
  "${GATEWAY_RPC_URL}" \
  "${GATEWAY_DA_PAIR_REPAIR_WAIT_ATTEMPTS}" \
  "${GATEWAY_DA_PAIR_REPAIR_WAIT_DELAY}"; then
  echo "gateway-launch: DA validator pair is still not set on Gateway for ${EDGE_CHAIN_NAME} after repair attempt" >&2
  exit 1
fi

ensure_gateway_commit_sender_validator "${EDGE_CHAIN_NAME}"
ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
