#!/usr/bin/env bash
# Migrate edge chain to Gateway settlement (§7).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_execute_operator_lock.sh"
MIGRATION_CHECK_ONLY=false
MIGRATION_PREFLIGHT_ONLY=false
MIGRATION_POST_FINALIZE_REPAIR=false
if [ "${1:-}" = "--check-only" ]; then
  MIGRATION_CHECK_ONLY=true
  shift
elif [ "${1:-}" = "--preflight" ]; then
  MIGRATION_PREFLIGHT_ONLY=true
  shift
elif [ "${1:-}" = "--resume-post-finalize" ]; then
  MIGRATION_POST_FINALIZE_REPAIR=true
  shift
fi
[ "$#" -eq 0 ] || gl_die "usage: edge-chain-migrate-to-gateway.sh [--check-only|--preflight|--resume-post-finalize]"
MIGRATION_READ_ONLY=false
if [ "${MIGRATION_CHECK_ONLY}" = true ] || [ "${MIGRATION_PREFLIGHT_ONLY}" = true ]; then
  MIGRATION_READ_ONLY=true
fi
MIGRATION_EXISTING_STATE_ONLY=false
if [ "${MIGRATION_READ_ONLY}" = true ] || [ "${MIGRATION_POST_FINALIZE_REPAIR}" = true ]; then
  MIGRATION_EXISTING_STATE_ONLY=true
fi
gl_require ZKSYNC_ERA_PATH
: "${EDGE_CHAIN_NAME:=zksys}"
gl_validate_zkstack_chain_name "${EDGE_CHAIN_NAME}" EDGE_CHAIN_NAME
: "${GATEWAY_DIR:=${HOME}/gateway}"
# SYSCOIN: A direct post-finalize repair must own the launch lock before any
# shared source patching, tool build, checkpoint read, or journal recovery.
if [ "${MIGRATION_POST_FINALIZE_REPAIR}" = true ]; then
  gl_acquire_gateway_launch_lock
fi
# SYSCOIN: Migrations target the single canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
: "${L1_RPC_URL:?L1_RPC_URL is required}"
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
cd "${GATEWAY_DIR}"

: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${GATEWAY_RPC_URL:=http://127.0.0.1:${GATEWAY_OS_RPC_PORT:-3052}}"
: "${GATEWAY_MAX_L1_GAS_PRICE:=1000000000}"
: "${GATEWAY_L2_DA_COMMITMENT_SCHEME:=BlobsZKsyncOS}"
: "${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE:=4}"
gl_normalize_canonical_deployment_inputs
gl_validate_l1_network_pair

gateway_governor_signer() {
  # SYSCOIN: fresh zkstack deployments generate a distinct Gateway governor
  # and persist it in the authenticated edge wallet. Use that signer unless an
  # operator explicitly selects an external account, keystore, hardware wallet,
  # or KMS identity.
  gl_to_lower "${EDGE_GATEWAY_GOVERNOR_SIGNER:-generated}"
}

validate_gateway_governor_signer_config() {
  local governor_signer account_name keystore_path password_file
  governor_signer="$(gateway_governor_signer)" || return $?

  if [ -n "${EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY:-}" ]; then
    echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY is intentionally unsupported; use a Foundry keystore account, keystore file, hardware wallet, or KMS signer" >&2
    return 1
  fi

  case "${governor_signer}" in
  generated | generated-wallet | wallet)
    password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
    if [ -z "${password_file}" ]; then
      command -v openssl >/dev/null 2>&1 || {
        echo "gateway-launch: openssl is required to protect the temporary generated-governor keystore" >&2
        return 1
      }
    fi
    command -v expect >/dev/null 2>&1 || {
      echo "gateway-launch: expect is required to import the generated governor key without exposing it in argv" >&2
      return 1
    }
    command -v cast >/dev/null 2>&1 || {
      echo "gateway-launch: cast is required to import the generated governor key" >&2
      return 1
    }
    ;;
  account)
    account_name="${EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME:-${FUNDER_ACCOUNT_NAME:-funder}}"
    [ -n "${account_name}" ] || {
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME must not be empty" >&2
      return 1
    }
    gl_validate_foundry_account_keystore \
      "${account_name}" "EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME"
    ;;
  keystore)
    keystore_path="${EDGE_GATEWAY_GOVERNOR_KEYSTORE:-${FUNDER_KEYSTORE:-}}"
    [ -n "${keystore_path}" ] || {
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_KEYSTORE is required when EDGE_GATEWAY_GOVERNOR_SIGNER=keystore" >&2
      return 1
    }
    gl_validate_secret_file "${keystore_path}" "governor keystore"
    ;;
  ledger | trezor | aws | gcp) ;;
  private-key)
    if gl_l1_network_requires_external_signer && ! gl_allow_insecure_private_key_argv; then
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_SIGNER=private-key is not allowed on ${L1_NETWORK}; use a Foundry account/keystore, hardware wallet, or KMS signer" >&2
      return 1
    fi
    if [ -z "${FUNDER_PRIVATE_KEY:-}" ] && gl_l1_network_requires_external_signer; then
      echo "gateway-launch: FUNDER_PRIVATE_KEY is required when inheriting EDGE_GATEWAY_GOVERNOR_SIGNER=private-key from FUNDER_SIGNER=private-key" >&2
      return 1
    fi
    ;;
  *)
    echo "gateway-launch: unsupported EDGE_GATEWAY_GOVERNOR_SIGNER=${governor_signer}; expected generated, account, keystore, ledger, trezor, aws, gcp, or private-key" >&2
    return 1
    ;;
  esac

  case "${governor_signer}" in
  generated | generated-wallet | wallet | account | keystore)
    password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
    if [ -n "${password_file}" ]; then
      gl_validate_secret_file "${password_file}" "governor password file"
    fi
    ;;
  esac
}

validate_migration_config_inputs() {
  local sender_min_balance
  sender_min_balance="${GATEWAY_SENDER_MIN_BALANCE_WEI:-${GATEWAY_COMMITTER_MIN_BALANCE_WEI:-100000000000000000000}}"
  # SYSCOIN: Parse every config-only value before gl.migration can advance.
  # Later repair/finalization paths consume only these already-validated shapes.
  python3 - \
    "${GATEWAY_MAX_L1_GAS_PRICE}" \
    "${GATEWAY_FUND_GOVERNOR_BALANCE_WEI:-11000000000000000000}" \
    "${sender_min_balance}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS:-120}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY:-5}" \
    "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS:-120}" \
    "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY:-5}" \
    "${GATEWAY_DA_PAIR_INITIAL_WAIT_ATTEMPTS:-4}" \
    "${GATEWAY_DA_PAIR_INITIAL_WAIT_DELAY:-2}" \
    "${GATEWAY_DA_PAIR_REPAIR_WAIT_ATTEMPTS:-120}" \
    "${GATEWAY_DA_PAIR_REPAIR_WAIT_DELAY:-5}" <<'PY' || return $?
import sys

UINT256_MAX = 2**256 - 1

def uint(raw: str, label: str, maximum: int) -> int:
    raw = raw.strip()
    try:
        value = int(raw, 16 if raw.lower().startswith("0x") else 10)
    except ValueError:
        raise SystemExit(f"invalid {label}: {raw!r}") from None
    if not 0 < value <= maximum:
        raise SystemExit(f"{label} must be between 1 and {maximum}")
    return value

for raw, label in zip(
    sys.argv[1:4],
    (
        "GATEWAY_MAX_L1_GAS_PRICE",
        "GATEWAY_FUND_GOVERNOR_BALANCE_WEI",
        "GATEWAY_SENDER_MIN_BALANCE_WEI",
    ),
):
    uint(raw, label, UINT256_MAX)
waits = (
    ("GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS", 1_000_000),
    ("GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY", 86_400),
    ("GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS", 1_000_000),
    ("GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY", 86_400),
    ("GATEWAY_DA_PAIR_INITIAL_WAIT_ATTEMPTS", 1_000_000),
    ("GATEWAY_DA_PAIR_INITIAL_WAIT_DELAY", 86_400),
    ("GATEWAY_DA_PAIR_REPAIR_WAIT_ATTEMPTS", 1_000_000),
    ("GATEWAY_DA_PAIR_REPAIR_WAIT_DELAY", 86_400),
)
for raw, (label, maximum) in zip(sys.argv[4:], waits):
    uint(raw, label, maximum)
PY
  "${SCRIPT_DIR}/provision-edge-settlement-fee-payer.sh" --validate-config-only || return $?
  if [ "${MIGRATION_CHECK_ONLY}" != true ]; then
    gl_validate_funder_signer_config || return $?
    validate_gateway_governor_signer_config || return $?
    command -v expect >/dev/null 2>&1 || {
      echo "gateway-launch: expect is required for settlement-fee payer provisioning" >&2
      return 1
    }
    command -v openssl >/dev/null 2>&1 || {
      echo "gateway-launch: openssl is required for settlement-fee payer provisioning" >&2
      return 1
    }
  fi
}

validate_migration_config_inputs

# SYSCOIN: These identities are compiled into the current guest and native server. A production
# deployment with different governance or salts must regenerate and repin all three atomically.
readonly SYSCOIN_COMPACT_EDGE_DA_RELAY="0x758b06cda80bdd016f79afd0df1a984039067a21"
readonly SYSCOIN_COMPACT_EDGE_DA_RELAY_RUNTIME_HASH="0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0"

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

# SYSCOIN: Post-finalize repair owns the launch lock above, but must only assert
# existing deployment state here rather than initialize or rewrite it.
if [ "${MIGRATION_POST_FINALIZE_REPAIR}" = true ]; then
  gl_assert_edge_launch_context
elif [ "${MIGRATION_EXISTING_STATE_ONLY}" = true ]; then
  gl_assert_edge_launch_context
else
  gl_bind_edge_launch_context
fi
gl_assert_gateway_runtime_identity
gl_assert_gateway_wrapped_base_token_pin "${GATEWAY_RPC_URL}"
gl_assert_edge_chain_config_matches_expected

# SYSCOIN: V32 nodes do not support live settlement-layer migration. Take the
# execute-operator/Gateway nonce lock before any migration/admin work and retain
# it through fee-payer provisioning and deposit unpause.
if [ "${MIGRATION_READ_ONLY}" != true ]; then
  gateway_acquire_execute_operator_lock "${EDGE_CHAIN_NAME}"
fi

gl_l1_broadcast_preflight

if [ "${MIGRATION_EXISTING_STATE_ONLY}" = true ]; then
  gl_probe_chain_contracts_schema_ready "${EDGE_CHAIN_NAME}" ||
    gl_die "edge contracts config is not ready for existing-state migration validation"
  gl_probe_chain_contracts_schema_ready "${GATEWAY_CHAIN_NAME}" ||
    gl_die "Gateway contracts config is not ready for existing-state migration validation"
else
  gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"
  gl_ensure_chain_contracts_yaml_schema "${GATEWAY_CHAIN_NAME}"
fi

# SYSCOIN: Direct invocation and repair must enforce the same L1 registration,
# owner, persisted-diamond, and operator-key bindings as the main launcher.
gl_assert_edge_chain_admin_owned_by_configured_governor
gl_authenticate_chain_wallet_roles \
  "${EDGE_CHAIN_NAME}" \
  "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" \
  prove_operator \
  execute_operator

configure_gateway_rpc_url_in_chain_secrets() {
  local chain_name="${1:?chain name required}"
  local gateway_rpc_url="${2:?gateway rpc url required}"
  local policy="${3:?secrets policy required}"
  local secrets_path="${GATEWAY_DIR}/chains/${chain_name}/configs/secrets.yaml"
  case "${policy}" in
  preflight | write | exact) ;;
  *) gl_die "invalid Gateway RPC secrets policy: ${policy}" ;;
  esac
  python3 - "${secrets_path}" "${gateway_rpc_url}" "${policy}" <<'PY'
import os
import stat
import sys
import tempfile
from pathlib import Path
import yaml

secrets_path = Path(sys.argv[1])
gateway_rpc_url = sys.argv[2]
policy = sys.argv[3]
if gateway_rpc_url == "" or gateway_rpc_url != gateway_rpc_url.strip():
    raise SystemExit("empty gateway rpc url")
try:
    parent_info = secrets_path.parent.lstat()
except FileNotFoundError:
    raise SystemExit(f"missing secrets config directory: {secrets_path.parent}")
if (
    stat.S_ISLNK(parent_info.st_mode)
    or not stat.S_ISDIR(parent_info.st_mode)
    or parent_info.st_uid != os.geteuid()
    or stat.S_IMODE(parent_info.st_mode) & 0o022
    or stat.S_IMODE(parent_info.st_mode) & 0o300 != 0o300
):
    raise SystemExit(f"unsafe secrets config directory ownership/mode: {secrets_path.parent}")
try:
    info = secrets_path.lstat()
except FileNotFoundError:
    raise SystemExit(f"missing secrets config: {secrets_path}")
if (
    stat.S_ISLNK(info.st_mode)
    or not stat.S_ISREG(info.st_mode)
    or info.st_uid != os.geteuid()
    or info.st_nlink != 1
    or stat.S_IMODE(info.st_mode) & 0o077
):
    raise SystemExit(f"unsafe secrets config ownership/mode: {secrets_path}")

flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
try:
    fd = os.open(secrets_path, flags)
except OSError as exc:
    raise SystemExit(f"cannot safely read secrets config {secrets_path}: {exc}") from exc
try:
    opened = os.fstat(fd)
    if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
        raise SystemExit(f"secrets config identity changed while opening: {secrets_path}")
    with os.fdopen(fd, "rb", closefd=True) as stream:
        raw = stream.read()
    fd = -1
finally:
    if fd >= 0:
        os.close(fd)

data = yaml.safe_load(raw.decode("utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {secrets_path}")

l1 = data.get("l1")
if l1 is None:
    l1 = {}
    data["l1"] = l1
if not isinstance(l1, dict):
    raise SystemExit(f"invalid l1 section in {secrets_path}")

current = l1.get("gateway_rpc_url")
if not isinstance(current, str) or current == "":
    if policy == "write":
        l1["gateway_rpc_url"] = gateway_rpc_url
        payload = yaml.safe_dump(
            data, sort_keys=False, allow_unicode=True
        ).encode("utf-8")
        temp_fd = -1
        temp_name = ""
        try:
            temp_fd, temp_name = tempfile.mkstemp(
                prefix=f".{secrets_path.name}.", dir=secrets_path.parent
            )
            os.fchmod(temp_fd, 0o600)
            with os.fdopen(temp_fd, "wb", closefd=True) as stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
            temp_fd = -1
            current_parent = secrets_path.parent.lstat()
            current_info = secrets_path.lstat()
            if (current_parent.st_dev, current_parent.st_ino) != (
                parent_info.st_dev,
                parent_info.st_ino,
            ):
                raise SystemExit(
                    f"secrets config directory identity changed: {secrets_path.parent}"
                )
            if (current_info.st_dev, current_info.st_ino) != (
                info.st_dev,
                info.st_ino,
            ):
                raise SystemExit(f"secrets config identity changed: {secrets_path}")
            os.replace(temp_name, secrets_path)
            temp_name = ""
            dir_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
            dir_fd = os.open(secrets_path.parent, dir_flags)
            try:
                os.fsync(dir_fd)
            finally:
                os.close(dir_fd)
        finally:
            if temp_fd >= 0:
                os.close(temp_fd)
            if temp_name:
                try:
                    os.unlink(temp_name)
                except FileNotFoundError:
                    pass
        print(
            f"gateway-launch: patched {secrets_path} "
            "(set l1.gateway_rpc_url=<redacted>)"
        )
    elif policy == "preflight":
        # Write mode uses an atomic sibling + replace, so patchability is the
        # authenticated parent's owner write/execute permission checked above;
        # the existing secret itself deliberately remains read-only here.
        pass
    else:
        raise SystemExit(f"missing l1.gateway_rpc_url in {secrets_path}")
elif current != gateway_rpc_url:
    raise SystemExit(
        f"Gateway RPC URL mismatch in {secrets_path}; "
        "refusing to migrate through a different Gateway"
    )
PY
}

if [ "${MIGRATION_PREFLIGHT_ONLY}" = true ]; then
  # SYSCOIN: Parse and authenticate existing secret inputs without patching
  # files while gl.migration is still pending.
  configure_gateway_rpc_url_in_chain_secrets "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" preflight
elif [ "${MIGRATION_CHECK_ONLY}" = true ] || [ "${MIGRATION_POST_FINALIZE_REPAIR}" = true ]; then
  configure_gateway_rpc_url_in_chain_secrets "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" exact
else
  configure_gateway_rpc_url_in_chain_secrets "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" write
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
  local chain_id bridgehub settlement_layer
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

  # SYSCOIN: cast transport failures may echo credential-bearing L1 URLs.
  if ! settlement_layer="$(cast call "${bridgehub}" \
    "settlementLayer(uint256)(uint256)" "${chain_id}" \
    --rpc-url "${L1_RPC_URL}" 2>/dev/null | awk 'NF { print $1; exit }')"; then
    echo "gateway-launch: failed to query the settlement layer for ${chain_name} on the configured L1 RPC" >&2
    return 1
  fi
  [ -n "${settlement_layer}" ] || {
    echo "gateway-launch: empty settlement layer for ${chain_name} from the configured L1 RPC" >&2
    return 1
  }
  printf '%s\n' "${settlement_layer}"
}

get_chain_diamond_proxy_from_gateway() {
  local chain_name="${1:?chain name required}"
  local chain_id call_from raw_proxy chain_proxy
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"
  if ! raw_proxy="$(gateway_cast_call_with_fallback "${L2_BRIDGEHUB_ADDRESS}" "getZKChain(uint256)(address)" "${GATEWAY_RPC_URL}" "${call_from}" "${chain_id}")"; then
    echo "gateway-launch: failed to query Gateway Bridgehub getZKChain(${chain_id}) for ${chain_name} on the configured Gateway RPC; target=${L2_BRIDGEHUB_ADDRESS}, from=${call_from:-unset}, cast=$(command -v cast || true)" >&2
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
  gateway_release_execute_operator_lock
}
trap cleanup_generated_gateway_governor_keystore EXIT

prepare_generated_gateway_governor_keystore() {
  local chain_name="${1:?chain name required}"
  local password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"
  local account_name="gateway-launch-generated-governor"
  local expected_addr imported_addr

  if [ -n "${password_file}" ]; then
    gl_validate_secret_file "${password_file}" "generated-governor password file"
  fi
  command -v expect >/dev/null 2>&1 || {
    echo "gateway-launch: expect is required to import the generated governor key without exposing it in argv" >&2
    return 1
  }

  if [ -z "${GATEWAY_GOVERNOR_TEMP_DIR}" ]; then
    GATEWAY_GOVERNOR_TEMP_DIR="$(mktemp -d)"
    chmod 700 "${GATEWAY_GOVERNOR_TEMP_DIR}"
    if [ -n "${password_file}" ]; then
      install -m 600 "${password_file}" "${GATEWAY_GOVERNOR_TEMP_DIR}/password"
    else
      # SYSCOIN: this keystore is process-local and short-lived; do not require
      # an unrelated funder credential merely to encrypt its generated key.
      openssl rand -hex -out "${GATEWAY_GOVERNOR_TEMP_DIR}/password" 32
      chmod 600 "${GATEWAY_GOVERNOR_TEMP_DIR}/password"
    fi

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
  imported_addr="$(gl_non_l1_cast wallet address --keystore "${GATEWAY_GOVERNOR_TEMP_DIR}/${account_name}" --password-file "${GATEWAY_GOVERNOR_TEMP_DIR}/password" | tr '[:upper:]' '[:lower:]')"
  if [ "${expected_addr}" != "${imported_addr}" ]; then
    echo "gateway-launch: generated-governor keystore mismatch: expected ${expected_addr}, got ${imported_addr}" >&2
    return 1
  fi

  GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--keystore "${GATEWAY_GOVERNOR_TEMP_DIR}/${account_name}" --password-file "${GATEWAY_GOVERNOR_TEMP_DIR}/password")
}

prepare_gateway_governor_forge_wallet_args() {
  GATEWAY_GOVERNOR_FORGE_WALLET_ARGS=()
  local governor_signer password_file

  if [ -n "${EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY:-}" ]; then
    echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY is intentionally unsupported; use a Foundry keystore account, keystore file, hardware wallet, or KMS signer" >&2
    return 1
  fi

  validate_gateway_governor_signer_config || return $?
  governor_signer="$(gateway_governor_signer)" || return $?
  password_file="${EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE:-${FUNDER_PASSWORD_FILE:-}}"

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
    gl_validate_foundry_account_keystore \
      "${account_name}" "EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME"
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--account "${account_name}")
    if [ -n "${password_file}" ]; then
      GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--password-file "${password_file}")
    fi
    ;;
  keystore)
    local keystore_path="${EDGE_GATEWAY_GOVERNOR_KEYSTORE:-${FUNDER_KEYSTORE:-}}"
    [ -n "${keystore_path}" ] || {
      echo "gateway-launch: EDGE_GATEWAY_GOVERNOR_KEYSTORE is required when EDGE_GATEWAY_GOVERNOR_SIGNER=keystore" >&2
      return 1
    }
    gl_validate_secret_file "${keystore_path}" "governor keystore"
    GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--keystore "${keystore_path}")
    if [ -n "${password_file}" ]; then
      GATEWAY_GOVERNOR_FORGE_WALLET_ARGS+=(--password-file "${password_file}")
    fi
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

}

assert_gateway_governor_signer_identity() {
  local expected_addr actual_addr
  prepare_gateway_governor_forge_wallet_args || return $?
  expected_addr="$(get_chain_governor_from_wallets "${EDGE_CHAIN_NAME}")" || return $?
  actual_addr="$(gl_non_l1_cast wallet address "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}")" || {
    echo "gateway-launch: failed to resolve the configured Gateway governor signer address" >&2
    return 1
  }
  expected_addr="$(gl_to_lower "${expected_addr}")"
  actual_addr="$(gl_to_lower "${actual_addr}")"
  [ "${actual_addr}" = "${expected_addr}" ] || {
    echo "gateway-launch: Gateway governor signer mismatch: expected ${expected_addr}, got ${actual_addr}" >&2
    return 1
  }
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

get_l1_edge_chain_admin_addr() {
  local resolved gateway_governor edge_governor edge_governor_matches
  local gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond raw_admin
  resolved="$(gl_edge_governor_reuse_context)" || return $?
  IFS='|' read -r gateway_governor edge_governor edge_governor_matches \
    gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond <<<"${resolved}"
  # SYSCOIN: bind the persisted replay sequence to the actual outer L1 target;
  # AdminFunctions routes the inner BridgeHub call through the edge ChainAdmin.
  raw_admin="$(cast call "${edge_diamond}" "getAdmin()(address)" \
    --rpc-url "${L1_RPC_URL}" 2>/dev/null)" || {
    echo "gateway-launch: failed to resolve the edge ChainAdmin on the configured L1 RPC" >&2
    return 1
  }
  gl_normalize_cast_address "edge ChainAdmin" "${raw_admin}"
}

# SYSCOIN: Read both migration postconditions from one L1 block so a concurrent
# migration transition cannot synthesize an already-finalized state.
gateway_migration_finalized_on_l1() {
  local chain_name="${1:?chain name required}"
  local gateway_chain_id="${2:?Gateway chain ID required}"
  local edge_chain_id bridgehub l1_block_number settlement_layer
  local chain_asset_handler_raw chain_asset_handler migration_in_progress

  case "${gateway_chain_id}" in
  "" | *[!0-9]*) gl_die "invalid Gateway chain ID: ${gateway_chain_id:-<empty>}" ;;
  esac
  edge_chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")" || \
    gl_die "failed to resolve chain ID for ${chain_name}"
  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")" || \
    gl_die "failed to resolve L1 BridgeHub for ${chain_name}"
  # SYSCOIN: cast transport failures may echo credential-bearing L1 URLs.
  # Discard raw stderr and retain only the bounded stage diagnostics below.
  l1_block_number="$(cast block-number --rpc-url "${L1_RPC_URL}" 2>/dev/null)" || \
    gl_die "failed to resolve an L1 block for ${chain_name} migration state"
  case "${l1_block_number}" in
  "" | *[!0-9]*) gl_die "invalid L1 block number returned for ${chain_name}: ${l1_block_number:-<empty>}" ;;
  esac
  settlement_layer="$(cast call "${bridgehub}" "settlementLayer(uint256)(uint256)" \
    "${edge_chain_id}" --rpc-url "${L1_RPC_URL}" --block "${l1_block_number}" 2>/dev/null | \
    awk 'NF { print $1; exit }')" || \
    gl_die "failed to query L1 settlement layer for ${chain_name} at block ${l1_block_number}"
  case "${settlement_layer}" in
  "" | *[!0-9]*) gl_die "invalid L1 settlement layer returned for ${chain_name}: ${settlement_layer:-<empty>}" ;;
  esac
  if [ "${settlement_layer}" != "${gateway_chain_id}" ]; then
    printf '%s\n' false
    return 0
  fi

  chain_asset_handler_raw="$(cast call "${bridgehub}" "chainAssetHandler()(address)" \
    --rpc-url "${L1_RPC_URL}" --block "${l1_block_number}" 2>/dev/null)" || \
    gl_die "failed to query L1 ChainAssetHandler for ${chain_name} at block ${l1_block_number}"
  chain_asset_handler="$(gl_normalize_cast_address "L1 ChainAssetHandler" "${chain_asset_handler_raw}")" || return $?
  [ "${chain_asset_handler}" != "0x0000000000000000000000000000000000000000" ] || \
    gl_die "L1 BridgeHub returned a zero ChainAssetHandler for ${chain_name}"
  migration_in_progress="$(cast call "${chain_asset_handler}" \
    "isMigrationInProgress(uint256)(bool)" "${edge_chain_id}" \
    --rpc-url "${L1_RPC_URL}" --block "${l1_block_number}" 2>/dev/null | \
    awk 'NF { print tolower($1); exit }')" || \
    gl_die "failed to query L1 migration state for ${chain_name} at block ${l1_block_number}"
  case "${migration_in_progress}" in
  false) printf '%s\n' true ;;
  true) printf '%s\n' false ;;
  *) gl_die "invalid L1 migration state returned for ${chain_name}: ${migration_in_progress:-<empty>}" ;;
  esac
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

  local out
  if [ -n "${call_from}" ]; then
    # SYSCOIN: read-only Gateway calls must not inherit L1 broadcast fee env.
    # Gateway can have a different base fee, and cast applies ETH_GAS_PRICE to
    # eth_call transactions even though no transaction is broadcast.
    if out="$(gl_non_l1_cast call "${target}" "${sig}" "$@" \
      --rpc-url "${rpc_url}" --from "${call_from}" 2>/dev/null)"; then
      printf '%s\n' "${out}"
      return 0
    fi
  fi
  if out="$(gl_non_l1_cast call "${target}" "${sig}" "$@" \
    --rpc-url "${rpc_url}" 2>/dev/null)"; then
    printf '%s\n' "${out}"
    return 0
  fi
  # SYSCOIN: cast transport errors can echo credential-bearing RPC URLs. Keep
  # that stderr out of the persistent launcher log and emit bounded context.
  echo "gateway-launch: cast call failed on the configured Gateway RPC: target=${target}, sig=${sig}, from=${call_from:-unset}, args=$*" >&2
  return 1
}

gateway_address_has_code() {
  local rpc_url="${1:?rpc url required}"
  local addr="${2:?address required}"

  local code
  if ! code="$(gl_non_l1_cast code "${addr}" --rpc-url "${rpc_url}" 2>/dev/null)"; then
    return 1
  fi
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ -n "${code}" ] || return 1
  [ "${code}" != "0x" ] || return 1
}

gateway_address_has_exact_runtime() {
  local rpc_url="${1:?rpc url required}"
  local addr="${2:?address required}"
  local expected_hash="${3:?runtime hash required}"
  local code actual_hash

  if ! code="$(gl_non_l1_cast code "${addr}" --rpc-url "${rpc_url}" 2>/dev/null)"; then
    return 1
  fi
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ -n "${code}" ] && [ "${code}" != "0x" ] || return 1
  actual_hash="$(cast keccak "${code}")" || return 1
  [ "$(gl_to_lower "${actual_hash}")" = "$(gl_to_lower "${expected_hash}")" ]
}

gateway_validator_role_set() {
  local chain_name="${1:?chain name required}"
  local validator_addr="${2:?validator address required}"
  local role_name="${3:?role name required}"
  local chain_id validator_timelock role call_from result
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  validator_timelock="$(get_gateway_validator_timelock_addr "${GATEWAY_CHAIN_NAME}")"
  call_from="$(get_chain_governor_from_wallets "${chain_name}")"

  role="$(gateway_cast_call_with_fallback \
    "${validator_timelock}" \
    "${role_name}()(bytes32)" \
    "${GATEWAY_RPC_URL}" \
    "${call_from}" | awk '{print $1}')"
  [ -n "${role}" ] || return 1

  result="$(gateway_cast_call_with_fallback \
    "${validator_timelock}" \
    "hasRoleForChainId(uint256,bytes32,address)(bool)" \
    "${GATEWAY_RPC_URL}" \
    "${call_from}" \
    "${chain_id}" \
    "${role}" \
    "${validator_addr}" | awk '{print $1}')" || return 1
  [ "${result}" = "true" ]
}

gateway_committer_role_set() {
  gateway_validator_role_set \
    "${1:?chain name required}" "${2:?committer address required}" COMMITTER_ROLE
}

gateway_required_validator_roles_ready() {
  local chain_name="${1:?chain name required}" spec wallet_name role_name validator_addr
  for spec in \
    "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}:COMMITTER_ROLE" \
    "prove_operator:PROVER_ROLE" \
    "execute_operator:EXECUTOR_ROLE"; do
    IFS=':' read -r wallet_name role_name <<<"${spec}"
    validator_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")" || return $?
    gateway_validator_role_set "${chain_name}" "${validator_addr}" "${role_name}" || return $?
  done
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

  gl_non_l1_cast balance "${committer_addr}" --rpc-url "${GATEWAY_RPC_URL}"
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

# SYSCOIN: A via-Gateway repair can be accepted on L1 before Forge reports
# success. Persist the exact dry-run nonce sequence before any send-capable
# Forge process starts, then use only --resume until the postcondition lands.
GATEWAY_ADMIN_REPAIR_DIR=""
GATEWAY_ADMIN_REPAIR_KEY=""
GATEWAY_ADMIN_REPAIR_FORGE_PATH=""
GATEWAY_ADMIN_REPAIR_FORGE_SHA256=""
GATEWAY_ADMIN_REPAIR_RESUME=false
GATEWAY_ADMIN_REPAIR_VALUE_WEI=""

gateway_admin_repair_foundry_identity() {
  local forge_path output version commit binary_sha
  forge_path="$(command -v forge)" || {
    echo "gateway-launch: Forge is required for via-Gateway repair" >&2
    return 1
  }
  if [[ "${forge_path}" != /* ]] || [ ! -f "${forge_path}" ] || [ -L "${forge_path}" ]; then
    echo "gateway-launch: via-Gateway repair requires an absolute regular Forge executable" >&2
    return 1
  fi
  if ! output="$("${forge_path}" --version 2>/dev/null)"; then
    echo "gateway-launch: failed to identify Forge for via-Gateway repair" >&2
    return 1
  fi
  version="$(printf '%s\n' "${output}" | awk '$1 == "forge" && $2 == "Version:" { print $3; exit }')"
  commit="$(printf '%s\n' "${output}" | awk '$1 == "Commit" && $2 == "SHA:" { print $3; exit }')"
  binary_sha="$(gl_sha256_file "${forge_path}")" || return $?
  # SYSCOIN: This journal relies on the audited sequence save/load order shared
  # by vanilla 1.7.1 and the contracts toolchain pinned by this repository.
  case "${version}|${commit}" in
  "1.7.1|4072e48705af9d93e3c0f6e29e93b5e9a40caed8" | \
  "1.3.5-foundry-zksync-v0.1.5|807f47ace7cdd90eed7190dc4481952cfaa25938") ;;
  "1.3.5-foundry-zksync-v0.1.5|VERGEN_IDEMPOTENT_OUTPUT")
    [ "${binary_sha}" = "789c539cc69ccbfbeee308b6305321edab651a327cca2c438961e3150448e987" ] || {
      echo "gateway-launch: unrecognized foundry-zksync v0.1.5 release binary for via-Gateway repair" >&2
      return 1
    }
    ;;
  *)
    echo "gateway-launch: via-Gateway repair requires audited Forge 1.7.1 or foundry-zksync v0.1.5" >&2
    return 1
    ;;
  esac
  [[ "${forge_path}" != *'|'* ]] || {
    echo "gateway-launch: unsupported Forge path for via-Gateway repair" >&2
    return 1
  }
  printf '%s|%s|%s|%s\n' "${version}" "${commit}" "${binary_sha}" "${forge_path}"
}

gateway_admin_repair_journal() {
  GATEWAY_ADMIN_REPAIR_RPC_URL="${L1_RPC_URL}" python3 - "$@" <<'PY'
import hashlib
import json
import os
import re
import shutil
import stat
import sys
from pathlib import Path

action = sys.argv[1]
state_dir = Path(sys.argv[2])
operation_key = sys.argv[3]
l1_chain_id = sys.argv[4]
arguments = sys.argv[5:]
rpc_url = os.environ.get("GATEWAY_ADMIN_REPAIR_RPC_URL", "")

if not re.fullmatch(
    r"(?:committer-role|sender-balance)-[1-9][0-9]*-0x[0-9a-f]{40}",
    operation_key,
):
    raise SystemExit(f"invalid via-Gateway repair operation key: {operation_key}")
if not re.fullmatch(r"[1-9][0-9]*", l1_chain_id):
    raise SystemExit(f"invalid L1 chain ID for via-Gateway repair: {l1_chain_id}")
if not rpc_url:
    raise SystemExit("missing L1 RPC URL for via-Gateway repair")

def present(path: Path) -> bool:
    return os.path.lexists(path)

def parse_arguments(items: list[str]) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for item in items:
        if "=" not in item:
            raise SystemExit(f"invalid via-Gateway repair argument: {item}")
        key, value = item.split("=", 1)
        if not re.fullmatch(r"[a-z][a-z0-9_]*", key) or not value or key in parsed:
            raise SystemExit(f"invalid via-Gateway repair argument: {item}")
        parsed[key] = value
    return parsed

def uint(value: object, label: str) -> int:
    if isinstance(value, bool):
        raise SystemExit(f"invalid {label} in via-Gateway repair intent")
    if isinstance(value, int):
        number = value
    elif isinstance(value, str) and re.fullmatch(r"(?:0x[0-9a-fA-F]+|[0-9]+)", value):
        number = int(value, 16 if value.lower().startswith("0x") else 10)
    else:
        raise SystemExit(f"invalid {label} in via-Gateway repair intent")
    if number < 0 or number >= 1 << 256:
        raise SystemExit(f"invalid {label} in via-Gateway repair intent")
    return number

def require_private_dir(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise SystemExit(f"missing via-Gateway repair directory: {path}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise SystemExit(f"unsafe via-Gateway repair directory: {path}")
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise SystemExit(f"unsafe via-Gateway repair directory ownership/mode: {path}")

def read_private_file(path: Path, *, exact_mode: int | None = None) -> bytes:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise SystemExit(f"missing via-Gateway repair file: {path}") from exc
    mode = stat.S_IMODE(info.st_mode)
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or info.st_uid != os.geteuid()
        or mode & 0o077
        or (exact_mode is not None and mode != exact_mode)
    ):
        raise SystemExit(f"unsafe via-Gateway repair file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        opened = os.fstat(fd)
        if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
            raise SystemExit(f"via-Gateway repair file identity changed: {path}")
        if opened.st_size > 16 * 1024 * 1024:
            raise SystemExit(f"oversized via-Gateway repair file: {path}")
        with os.fdopen(fd, "rb", closefd=True) as stream:
            return stream.read()
    finally:
        try:
            os.close(fd)
        except OSError:
            pass

def load_json(path: Path, *, exact_mode: int | None = None) -> tuple[bytes, object]:
    raw = read_private_file(path, exact_mode=exact_mode)
    try:
        return raw, json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise SystemExit(f"invalid via-Gateway repair JSON: {path}") from exc

def fsync_dir(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

def safe_rmtree(path: Path) -> None:
    if not present(path):
        return
    require_private_dir(path)
    for child in path.rglob("*"):
        info = child.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
            raise SystemExit(f"unsafe via-Gateway repair cleanup entry: {child}")
        if not stat.S_ISDIR(info.st_mode) and (
            not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
        ):
            raise SystemExit(f"unsafe via-Gateway repair cleanup entry: {child}")
    shutil.rmtree(path)

def validate_intent_arguments(items: list[str]) -> tuple[dict[str, str], int]:
    parsed = parse_arguments(items)
    kind = parsed.get("kind")
    expected = "committer-role" if operation_key.startswith("committer-role-") else "sender-balance"
    if kind != expected:
        raise SystemExit(f"invalid via-Gateway repair kind for {operation_key}")
    required = (
        "value_wei", "signer", "bridgehub", "l1_admin", "l1_chain_id",
        "foundry_version", "foundry_commit", "foundry_sha256",
    )
    if any(not parsed.get(key) for key in required) or parsed["l1_chain_id"] != l1_chain_id:
        raise SystemExit(f"incomplete via-Gateway repair intent for {operation_key}")
    for key in ("signer", "bridgehub", "l1_admin"):
        if not re.fullmatch(r"0x[0-9a-f]{40}", parsed[key]):
            raise SystemExit(f"invalid {key} in via-Gateway repair intent")
    if not re.fullmatch(r"[0-9a-f]{64}", parsed["foundry_sha256"]):
        raise SystemExit("invalid foundry_sha256 in via-Gateway repair intent")
    value = uint(parsed["value_wei"], "value_wei")
    if kind == "committer-role" and value != 0:
        raise SystemExit("committer-role via-Gateway repair must not transfer value")
    if kind == "sender-balance":
        observed = uint(parsed.get("observed_balance_wei"), "observed_balance_wei")
        minimum = uint(parsed.get("minimum_balance_wei"), "minimum_balance_wei")
        if observed + value != minimum:
            raise SystemExit("sender-balance repair value does not match its observed deficit")
    return parsed, value

root = state_dir / "via-gateway-repairs"
operation_dir = root / operation_key
broadcast_dir = operation_dir / "broadcast"
cache_dir = operation_dir / "cache"
intent_path = operation_dir / "intent.json"
prepared_path = operation_dir / "prepared.json"
preparing_path = operation_dir / ".preparing.json"
sequence_name = "adminL1L2TxViaGateway-latest.json"
sequence_base = Path("AdminFunctions.s.sol") / l1_chain_id

def sequence_path(base: Path, dry_run: bool) -> Path:
    path = base / sequence_base
    if dry_run:
        path /= "dry-run"
    return path / sequence_name

def inspect_sequence_tree(base: Path) -> tuple[list[Path], list[Path]]:
    require_private_dir(base)
    script_dir = base / "AdminFunctions.s.sol"
    if not present(script_dir):
        return [], []
    require_private_dir(script_dir)
    for child in script_dir.iterdir():
        if child.name != l1_chain_id:
            raise SystemExit(f"unexpected via-Gateway repair sequence entry: {child}")
    chain_dir = script_dir / l1_chain_id
    if not present(chain_dir):
        return [], []
    require_private_dir(chain_dir)
    normal: list[Path] = []
    dry: list[Path] = []
    for child in chain_dir.iterdir():
        if child.name == "dry-run":
            require_private_dir(child)
            for item in child.iterdir():
                if not (item.name == sequence_name or re.fullmatch(r"run-[0-9]+\.json", item.name)):
                    raise SystemExit(f"unexpected via-Gateway repair sequence entry: {item}")
                read_private_file(item)
                dry.append(item)
        else:
            if not (child.name == sequence_name or re.fullmatch(r"run-[0-9]+\.json", child.name)):
                raise SystemExit(f"unexpected via-Gateway repair sequence entry: {child}")
            read_private_file(child)
            normal.append(child)
    return normal, dry

def projection(data: dict[str, object]) -> dict[str, object]:
    projected = dict(data)
    projected.pop("timestamp", None)
    projected.pop("receipts", None)
    projected.pop("pending", None)
    txs = []
    for item in projected.get("transactions", []):
        tx = dict(item)
        tx.pop("hash", None)
        txs.append(tx)
    projected["transactions"] = txs
    return projected

def load_sequence_pair(dry_run: bool, intent_arguments: dict[str, str]) -> tuple[bytes, bytes, dict[str, object]]:
    public_path = sequence_path(broadcast_dir, dry_run)
    sensitive_path = sequence_path(cache_dir, dry_run)
    if present(public_path) != present(sensitive_path):
        raise SystemExit(f"partial via-Gateway repair sequence for {operation_key}")
    public_raw, public = load_json(public_path)
    sensitive_raw, sensitive = load_json(sensitive_path)
    required_top = {"transactions", "receipts", "pending", "libraries", "returns", "timestamp", "chain", "commit"}
    if not isinstance(public, dict) or not required_top.issubset(public):
        raise SystemExit(f"invalid public via-Gateway repair sequence: {public_path}")
    txs = public.get("transactions")
    if not isinstance(txs, list) or len(txs) != 1 or not isinstance(txs[0], dict):
        raise SystemExit(f"invalid transaction count in via-Gateway repair sequence: {public_path}")
    entry = txs[0]
    request = entry.get("transaction")
    if entry.get("transactionType") != "CALL" or not isinstance(request, dict):
        raise SystemExit(f"invalid transaction in via-Gateway repair sequence: {public_path}")
    if uint(public.get("chain"), "sequence chain") != int(l1_chain_id):
        raise SystemExit(f"wrong chain in via-Gateway repair sequence: {public_path}")
    if request.get("chainId") is not None and uint(request["chainId"], "transaction chainId") != int(l1_chain_id):
        raise SystemExit(f"wrong transaction chain in via-Gateway repair sequence: {public_path}")
    if str(request.get("from", "")).lower() != intent_arguments["signer"]:
        raise SystemExit(f"wrong signer in via-Gateway repair sequence: {public_path}")
    if str(request.get("to", "")).lower() != intent_arguments["l1_admin"]:
        raise SystemExit(f"wrong target in via-Gateway repair sequence: {public_path}")
    if request.get("nonce") is None or uint(request["nonce"], "transaction nonce") >= 1 << 64:
        raise SystemExit(f"invalid nonce in via-Gateway repair sequence: {public_path}")
    calldata_keys = [key for key in ("input", "data") if key in request]
    if len(calldata_keys) != 1:
        raise SystemExit(f"ambiguous calldata in via-Gateway repair sequence: {public_path}")
    tx_data = request[calldata_keys[0]]
    if not isinstance(tx_data, str) or not re.fullmatch(r"0x[0-9a-fA-F]{8,}", tx_data):
        raise SystemExit(f"invalid calldata in via-Gateway repair sequence: {public_path}")
    if dry_run and (entry.get("hash") is not None or public.get("receipts") != [] or public.get("pending") != []):
        raise SystemExit(f"non-pristine dry-run via-Gateway repair sequence: {public_path}")
    expected_sensitive = {"transactions": [{"rpc": rpc_url}]}
    if sensitive != expected_sensitive:
        raise SystemExit(f"invalid sensitive via-Gateway repair sequence: {sensitive_path}")
    return public_raw, sensitive_raw, projection(public)

def load_saved_intent() -> tuple[dict[str, object], dict[str, str], int]:
    _, saved = load_json(intent_path, exact_mode=0o600)
    if (
        not isinstance(saved, dict)
        or saved.get("schema_version") != 2
        or not isinstance(saved.get("checkpoint_run_id"), str)
        or not saved.get("checkpoint_run_id")
        or not isinstance(saved.get("checkpoint_fingerprint"), dict)
        or not saved.get("checkpoint_fingerprint")
        or not isinstance(saved.get("arguments"), list)
        or not all(isinstance(item, str) for item in saved["arguments"])
    ):
        raise SystemExit(f"invalid via-Gateway repair intent: {intent_path}")
    parsed, value = validate_intent_arguments(saved["arguments"])
    return saved, parsed, value

def load_marker() -> dict[str, str]:
    _, marker = load_json(prepared_path, exact_mode=0o600)
    if not isinstance(marker, dict) or set(marker) != {"schema_version", "public_sha256", "sensitive_sha256"} or marker.get("schema_version") != 1:
        raise SystemExit(f"invalid via-Gateway repair marker: {prepared_path}")
    for key in ("public_sha256", "sensitive_sha256"):
        if not isinstance(marker.get(key), str) or not re.fullmatch(r"[0-9a-f]{64}", marker[key]):
            raise SystemExit(f"invalid via-Gateway repair marker: {prepared_path}")
    return marker

if action == "begin":
    if not arguments:
        raise SystemExit("missing checkpoint state file for via-Gateway repair")
    state_file = Path(arguments[0])
    current_items = arguments[1:]
    current_arguments, current_value = validate_intent_arguments(current_items)
    require_private_dir(state_dir)
    _, state = load_json(state_file)
    run_id = state.get("run_id") if isinstance(state, dict) else None
    fingerprint = state.get("fingerprint") if isinstance(state, dict) else None
    if not isinstance(run_id, str) or not run_id or not isinstance(fingerprint, dict) or not fingerprint:
        raise SystemExit("checkpoint state is missing its authenticated run/fingerprint")
    if not present(root):
        root.mkdir(mode=0o700)
        os.chmod(root, 0o700)
        fsync_dir(state_dir)
    require_private_dir(root)
    staging = root / f".creating-{operation_key}"
    completed = root / f".completed-{operation_key}"
    for stale in (staging, completed):
        if present(stale):
            safe_rmtree(stale)
            fsync_dir(root)
    expected_intent = {
        "schema_version": 2,
        "checkpoint_run_id": run_id,
        "checkpoint_fingerprint": fingerprint,
        "arguments": current_items,
    }
    if not present(operation_dir):
        staging.mkdir(mode=0o700)
        os.chmod(staging, 0o700)
        for name in ("broadcast", "cache"):
            child = staging / name
            child.mkdir(mode=0o700)
            os.chmod(child, 0o700)
            fsync_dir(child)
        encoded = (json.dumps(expected_intent, indent=2, sort_keys=True) + "\n").encode()
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(staging / "intent.json", flags, 0o600)
        try:
            with os.fdopen(fd, "wb", closefd=True) as stream:
                stream.write(encoded)
                stream.flush()
                os.fsync(stream.fileno())
            fd = -1
        finally:
            if fd >= 0:
                os.close(fd)
        fsync_dir(staging)
        os.rename(staging, operation_dir)
        fsync_dir(root)
    require_private_dir(operation_dir)
    require_private_dir(broadcast_dir)
    require_private_dir(cache_dir)
    saved_intent, saved_arguments, saved_value = load_saved_intent()
    if saved_intent["checkpoint_run_id"] != run_id or saved_intent["checkpoint_fingerprint"] != fingerprint:
        raise SystemExit(f"via-Gateway repair checkpoint changed for {operation_key}; refusing replay")
    mutable = {"value_wei", "observed_balance_wei"} if current_arguments["kind"] == "sender-balance" else set()
    saved_identity = {key: value for key, value in saved_arguments.items() if key not in mutable}
    current_identity = {key: value for key, value in current_arguments.items() if key not in mutable}
    if saved_identity != current_identity:
        raise SystemExit(f"via-Gateway repair intent changed for {operation_key}; refusing replay")
    if current_arguments["kind"] == "sender-balance":
        saved_observed = uint(saved_arguments["observed_balance_wei"], "saved observed_balance_wei")
        current_observed = uint(current_arguments["observed_balance_wei"], "current observed_balance_wei")
        if current_observed < saved_observed:
            raise SystemExit(f"Gateway sender balance decreased outside the repair lock for {operation_key}")
    normal_files, _ = inspect_sequence_tree(broadcast_dir)
    normal_sensitive, _ = inspect_sequence_tree(cache_dir)
    if present(preparing_path):
        read_private_file(preparing_path, exact_mode=0o600)
        preparing_path.unlink()
        fsync_dir(operation_dir)
    if not present(prepared_path):
        if normal_files or normal_sensitive:
            raise SystemExit(f"unsealed broadcast sequence for {operation_key}; refusing replay")
        print(f"prepare|{saved_value}")
    else:
        marker = load_marker()
        dry_public, dry_sensitive, dry_projection = load_sequence_pair(True, saved_arguments)
        if hashlib.sha256(dry_public).hexdigest() != marker["public_sha256"] or hashlib.sha256(dry_sensitive).hexdigest() != marker["sensitive_sha256"]:
            raise SystemExit(f"sealed via-Gateway repair sequence changed for {operation_key}")
        normal_public = sequence_path(broadcast_dir, False)
        normal_cache = sequence_path(cache_dir, False)
        if present(normal_public) != present(normal_cache):
            raise SystemExit(f"partial broadcast via-Gateway repair sequence for {operation_key}")
        if present(normal_public):
            _, _, normal_projection = load_sequence_pair(False, saved_arguments)
            if normal_projection != dry_projection:
                raise SystemExit(f"broadcast via-Gateway repair sequence changed for {operation_key}")
        print(f"resume|{saved_value}")
elif action == "seal":
    require_private_dir(state_dir)
    require_private_dir(root)
    require_private_dir(operation_dir)
    require_private_dir(broadcast_dir)
    require_private_dir(cache_dir)
    _, saved_arguments, _ = load_saved_intent()
    normal_files, dry_files = inspect_sequence_tree(broadcast_dir)
    normal_sensitive, dry_sensitive_files = inspect_sequence_tree(cache_dir)
    if normal_files or normal_sensitive:
        raise SystemExit(f"broadcast sequence exists before sealing {operation_key}")
    public_raw, sensitive_raw, _ = load_sequence_pair(True, saved_arguments)
    if present(prepared_path):
        marker = load_marker()
        if marker["public_sha256"] != hashlib.sha256(public_raw).hexdigest() or marker["sensitive_sha256"] != hashlib.sha256(sensitive_raw).hexdigest():
            raise SystemExit(f"sealed via-Gateway repair sequence changed for {operation_key}")
        raise SystemExit(0)
    if present(preparing_path):
        read_private_file(preparing_path, exact_mode=0o600)
        preparing_path.unlink()
    for path in sorted(set(dry_files + dry_sensitive_files), key=lambda item: len(item.parts), reverse=True):
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    dirs = {
        broadcast_dir, cache_dir,
        broadcast_dir / sequence_base, cache_dir / sequence_base,
        broadcast_dir / sequence_base / "dry-run", cache_dir / sequence_base / "dry-run",
        broadcast_dir / "AdminFunctions.s.sol", cache_dir / "AdminFunctions.s.sol",
        operation_dir,
    }
    for path in sorted(dirs, key=lambda item: len(item.parts), reverse=True):
        fsync_dir(path)
    marker = {
        "schema_version": 1,
        "public_sha256": hashlib.sha256(public_raw).hexdigest(),
        "sensitive_sha256": hashlib.sha256(sensitive_raw).hexdigest(),
    }
    encoded = (json.dumps(marker, indent=2, sort_keys=True) + "\n").encode()
    fd = os.open(preparing_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(fd, "wb", closefd=True) as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        fd = -1
    finally:
        if fd >= 0:
            os.close(fd)
    os.replace(preparing_path, prepared_path)
    fsync_dir(operation_dir)
elif action == "finish":
    require_private_dir(state_dir)
    if not present(root):
        raise SystemExit(0)
    require_private_dir(root)
    completed = root / f".completed-{operation_key}"
    if present(operation_dir) and present(completed):
        raise SystemExit(f"ambiguous completed via-Gateway repair state for {operation_key}")
    if present(operation_dir):
        require_private_dir(operation_dir)
        os.rename(operation_dir, completed)
        fsync_dir(root)
    if present(completed):
        safe_rmtree(completed)
        fsync_dir(root)
else:
    raise SystemExit(f"invalid via-Gateway repair journal action: {action}")
PY
}

begin_gateway_admin_repair() {
  local operation_key="${1:?operation key required}"
  shift
  local state_dir state_file result mode saved_value extra
  local foundry_identity foundry_version foundry_commit foundry_sha foundry_path foundry_extra
  GATEWAY_ADMIN_REPAIR_DIR=""
  GATEWAY_ADMIN_REPAIR_KEY=""
  GATEWAY_ADMIN_REPAIR_FORGE_PATH=""
  GATEWAY_ADMIN_REPAIR_FORGE_SHA256=""
  GATEWAY_ADMIN_REPAIR_RESUME=false
  GATEWAY_ADMIN_REPAIR_VALUE_WEI=""
  foundry_identity="$(gateway_admin_repair_foundry_identity)" || return $?
  IFS='|' read -r foundry_version foundry_commit foundry_sha foundry_path foundry_extra <<<"${foundry_identity}"
  if [ -n "${foundry_extra:-}" ] || [ -z "${foundry_version}" ] || [ -z "${foundry_commit}" ] || [ -z "${foundry_sha}" ] || [ -z "${foundry_path}" ]; then
    echo "gateway-launch: malformed audited Forge identity" >&2
    return 1
  fi
  set -- "$@" "foundry_version=${foundry_version}" "foundry_commit=${foundry_commit}" "foundry_sha256=${foundry_sha}"
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  state_file="$(gl_checkpoint_state_file)" || return $?
  result="$(gateway_admin_repair_journal begin "${state_dir}" "${operation_key}" "${L1_CHAIN_ID}" "${state_file}" "$@")" || return $?
  IFS='|' read -r mode saved_value extra <<<"${result}"
  if [ -n "${extra:-}" ] || [[ ! "${saved_value}" =~ ^[0-9]+$ ]]; then
    echo "gateway-launch: malformed via-Gateway repair journal result" >&2
    return 1
  fi
  GATEWAY_ADMIN_REPAIR_DIR="${state_dir}/via-gateway-repairs/${operation_key}"
  GATEWAY_ADMIN_REPAIR_KEY="${operation_key}"
  GATEWAY_ADMIN_REPAIR_FORGE_PATH="${foundry_path}"
  GATEWAY_ADMIN_REPAIR_FORGE_SHA256="${foundry_sha}"
  GATEWAY_ADMIN_REPAIR_VALUE_WEI="${saved_value}"
  case "${mode}" in
  prepare) GATEWAY_ADMIN_REPAIR_RESUME=false ;;
  resume) GATEWAY_ADMIN_REPAIR_RESUME=true ;;
  *)
    echo "gateway-launch: invalid via-Gateway repair journal mode: ${mode}" >&2
    return 1
    ;;
  esac
}

seal_gateway_admin_repair() {
  local state_dir
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  gateway_admin_repair_journal seal \
    "${state_dir}" \
    "${GATEWAY_ADMIN_REPAIR_KEY:?repair journal is not initialized}" \
    "${L1_CHAIN_ID}"
}

finish_gateway_admin_repair() {
  local operation_key="${1:?operation key required}" state_dir
  GATEWAY_ADMIN_REPAIR_DIR=""
  GATEWAY_ADMIN_REPAIR_KEY=""
  GATEWAY_ADMIN_REPAIR_FORGE_PATH=""
  GATEWAY_ADMIN_REPAIR_FORGE_SHA256=""
  GATEWAY_ADMIN_REPAIR_RESUME=false
  GATEWAY_ADMIN_REPAIR_VALUE_WEI=""
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  gateway_admin_repair_journal finish \
    "${state_dir}" "${operation_key}" "${L1_CHAIN_ID}"
}

run_gateway_admin_repair_forge() {
  local phase
  if [ "${GATEWAY_ADMIN_REPAIR_RESUME}" = true ]; then
    phase=resume
  else
    phase=prepare
  fi
  while :; do
    [ "$(gl_sha256_file "${GATEWAY_ADMIN_REPAIR_FORGE_PATH:?repair Forge is not initialized}")" = \
      "${GATEWAY_ADMIN_REPAIR_FORGE_SHA256:?repair Forge hash is not initialized}" ] || {
      echo "gateway-launch: Forge changed after the via-Gateway repair intent was bound" >&2
      return 1
    }
    (
      umask 077
      export FOUNDRY_BROADCAST="${GATEWAY_ADMIN_REPAIR_DIR:?repair journal is not initialized}/broadcast"
      export FOUNDRY_CACHE_PATH="${GATEWAY_ADMIN_REPAIR_DIR}/cache"
      cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts" || exit $?
      if [ "${phase}" = resume ]; then
        set -- "$@" --resume
      fi
      exec "${GATEWAY_ADMIN_REPAIR_FORGE_PATH}" script deploy-scripts/AdminFunctions.s.sol:AdminFunctions \
        "$@" \
        --rpc-url "${L1_RPC_URL}" \
        "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}" \
        --slow
    ) || return $?
    [ "${phase}" = prepare ] || return 0
    seal_gateway_admin_repair || return $?
    GATEWAY_ADMIN_REPAIR_RESUME=true
    phase=resume
  done
}

fund_l1_governor_for_gateway_sender_deposits() {
  local chain_name="${1:?chain name required}"
  local min_balance_wei="${2:?minimum Gateway sender balance required}"
  local wallet_name sender_addr current_balance_wei total_top_up_wei
  local chain_wallet governor_base_wei governor_target_wei

  total_top_up_wei=0
  for wallet_name in "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" prove_operator execute_operator; do
    sender_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")"
    current_balance_wei="$(gateway_commit_sender_balance_wei "${sender_addr}")"
    total_top_up_wei="$(python3 - "${total_top_up_wei}" "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

total = int(sys.argv[1], 10)
current = int(sys.argv[2], 10)
target = int(sys.argv[3], 10)
print(total + max(0, target - current))
PY
)"
  done

  [ "${total_top_up_wei}" != "0" ] || return 0

  chain_wallet="${GATEWAY_DIR}/chains/${chain_name}/configs/wallets.yaml"
  [ -f "${chain_wallet}" ] || {
    echo "gateway-launch: missing chain wallet file for governor funding: ${chain_wallet}" >&2
    return 1
  }

  governor_base_wei="${GATEWAY_FUND_GOVERNOR_BALANCE_WEI:-11000000000000000000}"
  governor_target_wei="$(python3 - "${governor_base_wei}" "${total_top_up_wei}" <<'PY'
import sys

print(int(sys.argv[1], 10) + int(sys.argv[2], 10))
PY
)"

  echo "gateway-launch: funding ${chain_name} governor on L1 to ${governor_target_wei} wei before Gateway sender deposits (required outgoing deposit value=${total_top_up_wei} wei)"
  WALLETS_YAML_PATHS="${chain_wallet}" \
    GATEWAY_FUND_GOVERNOR_BALANCE_WEI="${governor_target_wei}" \
    gl_fund_wallets_yaml
}

ensure_gateway_commit_sender_validator() {
  local chain_name="${1:?chain name required}"
  local wallet_name committer_addr bridgehub validator_timelock refund_recipient chain_id gateway_chain_id chain_proxy committer_role grant_calldata
  local operation_key admin_script_sha forge_rc

  # zkstack migration currently enables only `operator`, while OS-server commits
  # Syscoin DA batches from `blob_operator`. Keep this in the launch layer until
  # upstream migration accepts the actual commit sender set.
  wallet_name="${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}"
  committer_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")"
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  operation_key="committer-role-${chain_id}-$(gl_to_lower "${committer_addr}")"

  if gateway_committer_role_set "${chain_name}" "${committer_addr}"; then
    finish_gateway_admin_repair "${operation_key}"
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
  admin_script_sha="$(gl_sha256_file "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/deploy-scripts/AdminFunctions.s.sol")"
  begin_gateway_admin_repair \
    "${operation_key}" \
    "kind=committer-role" \
    "contracts_sha=${REQUIRED_CONTRACTS_SHA}" \
    "admin_script_sha256=${admin_script_sha}" \
    "l1_chain_id=${L1_CHAIN_ID}" \
    "admin_chain_id=${chain_id}" \
    "destination_chain_id=${gateway_chain_id}" \
    "signer=$(gl_to_lower "${refund_recipient}")" \
    "bridgehub=$(gl_to_lower "${bridgehub}")" \
    "l1_admin=${edge_l1_chain_admin_addr}" \
    "max_l1_gas_price=${GATEWAY_MAX_L1_GAS_PRICE}" \
    "target=$(gl_to_lower "${validator_timelock}")" \
    "value_wei=0" \
    "calldata=$(gl_to_lower "${grant_calldata}")" \
    "refund_recipient=$(gl_to_lower "${refund_recipient}")" \
    "chain_proxy=$(gl_to_lower "${chain_proxy}")" \
    "role=$(gl_to_lower "${committer_role}")" \
    "committer=$(gl_to_lower "${committer_addr}")"

  if run_gateway_admin_repair_forge \
    --sig 'adminL1L2TxViaGateway(address,uint256,uint256,uint256,address,uint256,bytes,address,bool)' \
    "${bridgehub}" \
    "${GATEWAY_MAX_L1_GAS_PRICE}" \
    "${chain_id}" \
    "${gateway_chain_id}" \
    "${validator_timelock}" \
    0 \
    "${grant_calldata}" \
    "${refund_recipient}" \
    true; then
    forge_rc=0
  else
    forge_rc=$?
  fi

  : "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS:=120}"
  : "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY:=5}"
  echo "gateway-launch: waiting for Gateway committer role repair (up to $((GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS * GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY))s)"
  if wait_for_gateway_committer_role \
    "${chain_name}" \
    "${committer_addr}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_ATTEMPTS}" \
    "${GATEWAY_COMMITTER_ROLE_REPAIR_WAIT_DELAY}"; then
    finish_gateway_admin_repair "${operation_key}"
    if [ "${forge_rc}" -ne 0 ]; then
      echo "gateway-launch: Forge exited ${forge_rc}, but the exact Gateway committer-role postcondition is confirmed"
    fi
    return 0
  fi
  echo "gateway-launch: Gateway committer role still missing for ${wallet_name} (${committer_addr}) after repair attempt; retaining the exact Forge repair journal" >&2
  [ "${forge_rc}" -eq 0 ] || return "${forge_rc}"
  return 1
}

ensure_gateway_commit_sender_balance() {
  local chain_name="${1:?chain name required}"
  local bridgehub refund_recipient chain_id gateway_chain_id min_balance_wei
  local wallet_name sender_addr current_balance_wei candidate_top_up_wei top_up_wei
  local operation_key admin_script_sha forge_rc

  # SYSCOIN: zksys settles on Gateway, so its Gateway L1-sender wallets need
  # Gateway base token for commit/prove/execute transactions. This is
  # funding-only; role grants are handled separately above.
  min_balance_wei="${GATEWAY_SENDER_MIN_BALANCE_WEI:-${GATEWAY_COMMITTER_MIN_BALANCE_WEI:-100000000000000000000}}"
  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")"
  refund_recipient="$(get_chain_governor_from_wallets "${chain_name}")"
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")"
  gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
  admin_script_sha="$(gl_sha256_file "${ZKSYNC_ERA_PATH}/contracts/l1-contracts/deploy-scripts/AdminFunctions.s.sol")"

  # SYSCOIN: the launcher and OS server share the execute-operator lock, so
  # these sender balances cannot drift from supported node spending here.
  fund_l1_governor_for_gateway_sender_deposits "${chain_name}" "${min_balance_wei}"

  for wallet_name in "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" prove_operator execute_operator; do
    sender_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")"
    current_balance_wei="$(gateway_commit_sender_balance_wei "${sender_addr}")"
    operation_key="sender-balance-${chain_id}-$(gl_to_lower "${sender_addr}")"

    if python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

raise SystemExit(0 if int(sys.argv[1], 10) >= int(sys.argv[2], 10) else 1)
PY
    then
      finish_gateway_admin_repair "${operation_key}"
      echo "gateway-launch: Gateway sender balance already funded for ${wallet_name} (${sender_addr}): ${current_balance_wei} wei"
      continue
    fi

    candidate_top_up_wei="$(python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

print(int(sys.argv[2], 10) - int(sys.argv[1], 10))
PY
)"

    gl_l1_broadcast_preflight
    prepare_gateway_governor_forge_wallet_args
    begin_gateway_admin_repair \
      "${operation_key}" \
      "kind=sender-balance" \
      "contracts_sha=${REQUIRED_CONTRACTS_SHA}" \
      "admin_script_sha256=${admin_script_sha}" \
      "l1_chain_id=${L1_CHAIN_ID}" \
      "admin_chain_id=${chain_id}" \
      "destination_chain_id=${gateway_chain_id}" \
      "signer=$(gl_to_lower "${refund_recipient}")" \
      "bridgehub=$(gl_to_lower "${bridgehub}")" \
      "l1_admin=${edge_l1_chain_admin_addr}" \
      "max_l1_gas_price=${GATEWAY_MAX_L1_GAS_PRICE}" \
      "target=$(gl_to_lower "${sender_addr}")" \
      "value_wei=${candidate_top_up_wei}" \
      "calldata=0x" \
      "refund_recipient=$(gl_to_lower "${refund_recipient}")" \
      "minimum_balance_wei=${min_balance_wei}" \
      "observed_balance_wei=${current_balance_wei}"
    top_up_wei="${GATEWAY_ADMIN_REPAIR_VALUE_WEI}"

    echo "gateway-launch: Gateway sender balance below minimum for ${wallet_name} (${sender_addr}): current=${current_balance_wei} wei, minimum=${min_balance_wei} wei; funding exact journal value ${top_up_wei} wei via L1->Gateway admin tx"

    if run_gateway_admin_repair_forge \
      --sig 'adminL1L2TxViaGateway(address,uint256,uint256,uint256,address,uint256,bytes,address,bool)' \
      "${bridgehub}" \
      "${GATEWAY_MAX_L1_GAS_PRICE}" \
      "${chain_id}" \
      "${gateway_chain_id}" \
      "${sender_addr}" \
      "${top_up_wei}" \
      "0x" \
      "${refund_recipient}" \
      true; then
      forge_rc=0
    else
      forge_rc=$?
    fi

    : "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS:=120}"
    : "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY:=5}"
    echo "gateway-launch: waiting for Gateway sender balance repair for ${wallet_name} (up to $((GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS * GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY))s)"
    if wait_for_gateway_commit_sender_balance \
      "${sender_addr}" \
      "${min_balance_wei}" \
      "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_ATTEMPTS}" \
      "${GATEWAY_COMMITTER_BALANCE_REPAIR_WAIT_DELAY}"; then
      finish_gateway_admin_repair "${operation_key}"
      if [ "${forge_rc}" -ne 0 ]; then
        echo "gateway-launch: Forge exited ${forge_rc}, but the exact Gateway sender-balance postcondition is confirmed for ${wallet_name}"
      fi
      continue
    fi
    echo "gateway-launch: Gateway sender balance still below ${min_balance_wei} wei for ${wallet_name} (${sender_addr}) after repair attempt; retaining the exact Forge repair journal" >&2
    [ "${forge_rc}" -eq 0 ] || return "${forge_rc}"
    return 1
  done
}

provision_gateway_settlement_fee_payer() {
  local chain_name="${1:?chain name required}"

  # SYSCOIN: An edge execute operator pays interop settlement fees in wrapped
  # Gateway base token. Provision and authenticate that opt-in before deposits
  # reopen, including on idempotent migration resumes.
    GATEWAY_DIR="${GATEWAY_DIR}" \
    GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME}" \
    GATEWAY_RPC_URL="${GATEWAY_RPC_URL}" \
    GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS="${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}" \
    GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD="${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" \
    EDGE_CHAIN_NAME="${chain_name}" \
    "${SCRIPT_DIR}/provision-edge-settlement-fee-payer.sh" "${chain_name}"
}

gateway_commit_sender_balances_ready() {
  local chain_name="${1:?chain name required}" wallet_name sender_addr current_balance_wei
  local min_balance_wei
  min_balance_wei="${GATEWAY_SENDER_MIN_BALANCE_WEI:-${GATEWAY_COMMITTER_MIN_BALANCE_WEI:-100000000000000000000}}"
  for wallet_name in "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" prove_operator execute_operator; do
    sender_addr="$(get_wallet_address_from_wallets "${chain_name}" "${wallet_name}")" || return $?
    current_balance_wei="$(gateway_commit_sender_balance_wei "${sender_addr}")" || return $?
    python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY' || return $?
import sys

current, minimum = map(int, sys.argv[1:])
raise SystemExit(0 if current >= minimum else 1)
PY
  done
}

gateway_settlement_fee_payer_ready() {
  local chain_name="${1:?chain name required}"
  GATEWAY_DIR="${GATEWAY_DIR}" \
    GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME}" \
    GATEWAY_RPC_URL="${GATEWAY_RPC_URL}" \
    GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS="${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}" \
    EDGE_CHAIN_NAME="${chain_name}" \
    "${SCRIPT_DIR}/provision-edge-settlement-fee-payer.sh" --check-only "${chain_name}"
}

l1_deposits_are_unpaused() {
  local chain_name="${1:?chain name required}" bridgehub chain_id chain_proxy paused
  bridgehub="$(get_l1_bridgehub_proxy_addr "${chain_name}")" || return $?
  chain_id="$(get_chain_id_from_zkstack_yaml "${chain_name}")" || return $?
  # SYSCOIN: keep credential-bearing L1 transport errors out of launcher logs.
  chain_proxy="$(cast call "${bridgehub}" "getZKChain(uint256)(address)" \
    "${chain_id}" --rpc-url "${L1_RPC_URL}" 2>/dev/null | \
    awk 'NF { print $1; exit }')" || {
    echo "gateway-launch: failed to read the edge chain proxy from the configured L1 RPC" >&2
    return 1
  }
  [[ "${chain_proxy}" =~ ^0x[0-9a-fA-F]{40}$ ]] || return 1
  [ "$(gl_to_lower "${chain_proxy}")" != "0x0000000000000000000000000000000000000000" ] || return 1
  paused="$(cast call "${chain_proxy}" "depositsPaused()(bool)" \
    --rpc-url "${L1_RPC_URL}" 2>/dev/null | awk 'NF { print tolower($1); exit }')" || {
    echo "gateway-launch: failed to read the edge deposit state from the configured L1 RPC" >&2
    return 1
  }
  [ "${paused}" = "false" ]
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
  [ "$(gl_to_lower "${line1}")" = "${SYSCOIN_COMPACT_EDGE_DA_RELAY}" ] || return 1
  gateway_address_has_exact_runtime \
    "${gateway_rpc}" \
    "${line1}" \
    "${SYSCOIN_COMPACT_EDGE_DA_RELAY_RUNTIME_HASH}" || return 1

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

# SYSCOIN: Require the compact-rollup validator recorded for this Gateway topology.
get_l1_da_validator_for_edge() {
  local edge_chain_name="${1:?edge chain name required}"
  local gateway_chain_name="${2:?gateway chain name required}"
  local gateway_rpc_url="${3:?gateway rpc url required}"

  local candidate
  candidate="$(python3 - \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" \
    "${GATEWAY_DIR}/chains/${edge_chain_name}/configs/genesis.yaml" <<'PY'
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

def require_address(value, field):
    value = norm(value)
    if value is None or value.lower() == "0x0000000000000000000000000000000000000000":
        raise SystemExit(f"missing canonical {field}")
    return value

gateway_cfg_path = Path(sys.argv[1])
genesis_cfg_path = Path(sys.argv[2])

if not genesis_cfg_path.exists():
    raise SystemExit(f"missing edge genesis config: {genesis_cfg_path}")
genesis_data = yaml.safe_load(genesis_cfg_path.read_text(encoding="utf-8"))
if not isinstance(genesis_data, dict):
    raise SystemExit(f"invalid edge genesis config: {genesis_cfg_path}")
commitment_mode = genesis_data.get("l1_batch_commit_data_generator_mode", "rollup")
if not isinstance(commitment_mode, str) or commitment_mode.strip().lower() != "rollup":
    raise SystemExit(
        "edge migration requires canonical compact rollup DA; "
        f"got l1_batch_commit_data_generator_mode={commitment_mode!r}"
    )

if not gateway_cfg_path.exists():
    raise SystemExit(f"missing Gateway config: {gateway_cfg_path}")
gateway_data = yaml.safe_load(gateway_cfg_path.read_text(encoding="utf-8"))
if not isinstance(gateway_data, dict):
    raise SystemExit(f"invalid Gateway config: {gateway_cfg_path}")
print(require_address(gateway_data.get("relayed_sl_da_validator"), "relayed_sl_da_validator"))
PY
)"

  [ -n "${candidate}" ] || {
    echo "missing canonical relayed_sl_da_validator for ${edge_chain_name}" >&2
    return 1
  }

  if [ "$(gl_to_lower "${candidate}")" != "${SYSCOIN_COMPACT_EDGE_DA_RELAY}" ]; then
    echo "configured relayed_sl_da_validator ${candidate} does not match the guest-bound relay ${SYSCOIN_COMPACT_EDGE_DA_RELAY} for ${edge_chain_name}" >&2
    return 1
  fi

  if gateway_address_has_exact_runtime \
    "${gateway_rpc_url}" \
    "${candidate}" \
    "${SYSCOIN_COMPACT_EDGE_DA_RELAY_RUNTIME_HASH}"; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  echo "canonical relayed_sl_da_validator ${candidate} has the wrong or missing runtime on the configured Gateway RPC for ${edge_chain_name}" >&2
  return 1
}

ensure_deposits_unpaused() {
  local chain_name="${1:?chain name required}"
  local unpause_output=""
  local unpause_output_lc=""

  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${chain_name}"
  if ! unpause_output="$(gl_zkstack_pty zkstack chain unpause-deposits \
    --chain "${chain_name}" \
    --l1-rpc-url "${L1_RPC_URL}" \
    -v 2>&1)"; then
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
  GATEWAY_FUND_EDGE_CONTEXT=true \
    GATEWAY_FUND_TARGET_CHAIN_NAME="${chain_name}" \
    "${SCRIPT_DIR}/fund-wallets.sh"
}

gateway_chain_id="$(get_chain_id_from_zkstack_yaml "${GATEWAY_CHAIN_NAME}")"
if [ "${MIGRATION_POST_FINALIZE_REPAIR}" = true ]; then
  # SYSCOIN: prove the complete L1 postcondition before any post-finalize
  # repair broadcast or temporary signer material is prepared.
  migration_finalized="$(gateway_migration_finalized_on_l1 \
    "${EDGE_CHAIN_NAME}" "${gateway_chain_id}")"
  [ "${migration_finalized}" = true ] || \
    gl_die "refusing post-finalize repair before the edge Gateway migration is finalized on L1"
  echo "gateway-launch: L1 migration is finalized; resuming only post-migration reconciliation for ${EDGE_CHAIN_NAME}"
else
  current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
fi
edge_committer_wallet_name="${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}"
edge_committer_addr="$(get_wallet_address_from_wallets "${EDGE_CHAIN_NAME}" "${edge_committer_wallet_name}")"

# SYSCOIN: Authenticate the guest-bound relay and its exact runtime before the
# launcher changes gl.migration from pending. Normal migration reuses this
# attested address for later DA-pair repair.
l1_da_validator_addr="$(get_l1_da_validator_for_edge "${EDGE_CHAIN_NAME}" "${GATEWAY_CHAIN_NAME}" "${GATEWAY_RPC_URL}")"
if [ "${MIGRATION_CHECK_ONLY}" != true ]; then
  # SYSCOIN: Reject a zero, overflowing, or cap-incompatible live settlement
  # fee before gl.migration can advance or a direct migration can broadcast;
  # edge registration is not needed for this read-only check.
  "${SCRIPT_DIR}/provision-edge-settlement-fee-payer.sh" --preflight-fee-target
  # SYSCOIN: Prove the selected external signer resolves to the authenticated
  # edge governor while gl.migration is still pending. Hardware/KMS signers may
  # require their normal read-only interaction here.
  assert_gateway_governor_signer_identity
fi

if [ "${MIGRATION_PREFLIGHT_ONLY}" = true ]; then
  echo "gateway-launch: read-only migration prerequisites are ready for ${EDGE_CHAIN_NAME}"
  exit 0
fi
if [ "${MIGRATION_CHECK_ONLY}" = true ]; then
  migration_finalized="$(gateway_migration_finalized_on_l1 \
    "${EDGE_CHAIN_NAME}" "${gateway_chain_id}")"
  [ "${migration_finalized}" = true ] ||
    gl_die "edge Gateway migration is not finalized on L1"
  is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" ||
    gl_die "edge Gateway DA pair is not ready"
  gateway_required_validator_roles_ready "${EDGE_CHAIN_NAME}" ||
    gl_die "edge Gateway validator roles are not ready"
  gateway_commit_sender_balances_ready "${EDGE_CHAIN_NAME}" ||
    gl_die "edge Gateway sender balances are below their required reserve"
  gateway_settlement_fee_payer_ready "${EDGE_CHAIN_NAME}" ||
    gl_die "edge Gateway settlement fee payer is not ready"
  l1_deposits_are_unpaused "${EDGE_CHAIN_NAME}" ||
    gl_die "edge L1 deposits remain paused"
  echo "gateway-launch: migration postconditions are ready for ${EDGE_CHAIN_NAME}"
  exit 0
fi

edge_l1_chain_admin_addr="$(get_l1_edge_chain_admin_addr)"
edge_l1_chain_admin_addr="$(gl_to_lower "${edge_l1_chain_admin_addr}")"
readonly edge_l1_chain_admin_addr

if [ "${MIGRATION_POST_FINALIZE_REPAIR}" != true ]; then
if [ "${current_settlement_layer}" != "${gateway_chain_id}" ]; then
  pause_output=""
  pause_output_lc=""
  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
  if ! pause_output="$(gl_zkstack_pty zkstack chain pause-deposits \
    --chain "${EDGE_CHAIN_NAME}" \
    --l1-rpc-url "${L1_RPC_URL}" \
    -v 2>&1)"; then
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
    --l1-rpc-url "${L1_RPC_URL}" \
    --gateway-rpc-url "${GATEWAY_RPC_URL}" \
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

# SYSCOIN: Replay must trust the complete L1 postcondition, never zkstack error
# text. Skip only the non-idempotent finalize call; post-migration repairs below
# still run on every recovery.
finalize_already_complete="$(gateway_migration_finalized_on_l1 \
  "${EDGE_CHAIN_NAME}" "${gateway_chain_id}")"
if [ "${finalize_already_complete}" = true ]; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} migration is already finalized on L1; skipping finalize CLI and continuing with post-migration repair"
else
  finalize_output=""
  gl_l1_broadcast_preflight
  refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
  if ! finalize_output="$(gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
    --chain "${EDGE_CHAIN_NAME}" \
    --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
    --l1-rpc-url "${L1_RPC_URL}" \
    --gateway-rpc-url "${GATEWAY_RPC_URL}" \
    --deploy-paymaster false 2>&1)"; then
    echo "${finalize_output}"
    finalize_already_complete="$(gateway_migration_finalized_on_l1 \
      "${EDGE_CHAIN_NAME}" "${gateway_chain_id}")"
    if [ "${finalize_already_complete}" = true ]; then
      echo "gateway-launch: finalize failed after L1 reached the complete migration postcondition; continuing with post-migration repair"
    else
      exit 1
    fi
  else
    echo "${finalize_output}"
  fi
fi
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
gateway_required_validator_roles_ready "${EDGE_CHAIN_NAME}" ||
  gl_die "edge Gateway prove/execute validator roles are incomplete; refusing to mark migration ready"
ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
provision_gateway_settlement_fee_payer "${EDGE_CHAIN_NAME}"
# SYSCOIN: restore the execute-operator native sender reserve after wrapping.
ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
