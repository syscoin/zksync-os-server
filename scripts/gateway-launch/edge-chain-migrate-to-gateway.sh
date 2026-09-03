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
if [ "${1:-}" = "--check-only" ]; then
  MIGRATION_CHECK_ONLY=true
  shift
elif [ "${1:-}" = "--preflight" ]; then
  MIGRATION_PREFLIGHT_ONLY=true
  shift
fi
[ "$#" -eq 0 ] || gl_die "usage: edge-chain-migrate-to-gateway.sh [--check-only|--preflight]"
MIGRATION_READ_ONLY=false
if [ "${MIGRATION_CHECK_ONLY}" = true ] || [ "${MIGRATION_PREFLIGHT_ONLY}" = true ]; then
  MIGRATION_READ_ONLY=true
fi
gl_require ZKSYNC_ERA_PATH
: "${EDGE_CHAIN_NAME:=zksys}"
gl_validate_zkstack_chain_name "${EDGE_CHAIN_NAME}" EDGE_CHAIN_NAME
# SYSCOIN: Migrations target the single canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
: "${GATEWAY_DIR:=${HOME}/gateway}"
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

# SYSCOIN: Direct migration entry points bind the exact edge context before
# acquiring its shared nonce lock or mutating state. Read-only validation only
# asserts the existing canonical fingerprint and zkstack edge index.
if [ "${MIGRATION_READ_ONLY}" = true ]; then
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

if [ "${MIGRATION_READ_ONLY}" = true ]; then
  gl_probe_chain_contracts_schema_ready "${EDGE_CHAIN_NAME}" ||
    gl_die "edge contracts config is not ready for read-only migration validation"
  gl_probe_chain_contracts_schema_ready "${GATEWAY_CHAIN_NAME}" ||
    gl_die "Gateway contracts config is not ready for read-only migration validation"
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
elif [ "${MIGRATION_CHECK_ONLY}" = true ]; then
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
    if out="$(gl_non_l1_cast call "${target}" "${sig}" "$@" \
      --rpc-url "${rpc_url}" --from "${call_from}" 2>&1)"; then
      printf '%s\n' "${out}"
      return 0
    fi
    last_error="${out}"
  fi
  if out="$(gl_non_l1_cast call "${target}" "${sig}" "$@" \
    --rpc-url "${rpc_url}" 2>&1)"; then
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
# success. Bind every repair to a private, immutable intent and resume that
# exact Forge journal after interruption; never guess whether a fresh replay is
# safe from the still-delayed Gateway postcondition.
GATEWAY_ADMIN_REPAIR_DIR=""
GATEWAY_ADMIN_REPAIR_RESUME=false

begin_gateway_admin_repair() {
  local operation_key="${1:?operation key required}"
  shift
  local state_dir state_file mode
  # SYSCOIN: A rejected second intent must not leave a prior repair runnable.
  GATEWAY_ADMIN_REPAIR_DIR=""
  GATEWAY_ADMIN_REPAIR_RESUME=false
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  state_file="$(gl_checkpoint_state_file)" || return $?
  mode="$(python3 - "${state_dir}" "${state_file}" "${operation_key}" "$@" <<'PY'
import json
import os
import re
import stat
import sys
from pathlib import Path

state_dir = Path(sys.argv[1])
state_file = Path(sys.argv[2])
operation_key = sys.argv[3]
arguments = sys.argv[4:]

if not re.fullmatch(
    r"(?:committer-role|sender-balance)-[1-9][0-9]*-0x[0-9a-f]{40}",
    operation_key,
):
    raise SystemExit(f"invalid via-Gateway repair operation key: {operation_key}")

def require_private_dir(path: Path) -> None:
    info = path.lstat()
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise SystemExit(f"unsafe via-Gateway repair directory: {path}")
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise SystemExit(f"unsafe via-Gateway repair directory ownership/mode: {path}")

require_private_dir(state_dir)
state_info = state_file.lstat()
if (
    stat.S_ISLNK(state_info.st_mode)
    or not stat.S_ISREG(state_info.st_mode)
    or state_info.st_nlink != 1
    or state_info.st_uid != os.geteuid()
    or stat.S_IMODE(state_info.st_mode) & 0o077
):
    raise SystemExit(f"unsafe checkpoint state file: {state_file}")
state = json.loads(state_file.read_text(encoding="utf-8"))
run_id = state.get("run_id")
fingerprint = state.get("fingerprint")
if not isinstance(run_id, str) or not run_id or not isinstance(fingerprint, dict) or not fingerprint:
    raise SystemExit("checkpoint state is missing its authenticated run/fingerprint")

root = state_dir / "via-gateway-repairs"
if not root.exists():
    root.mkdir(mode=0o700)
    os.chmod(root, 0o700)
    parent_fd = os.open(state_dir, os.O_RDONLY)
    try:
        os.fsync(parent_fd)
    finally:
        os.close(parent_fd)
require_private_dir(root)

operation_dir = root / operation_key
created = False
if not operation_dir.exists():
    operation_dir.mkdir(mode=0o700)
    os.chmod(operation_dir, 0o700)
    created = True
require_private_dir(operation_dir)

broadcast_dir = operation_dir / "broadcast"
intent_path = operation_dir / "intent.json"
intent = {
    "schema_version": 1,
    "checkpoint_run_id": run_id,
    "checkpoint_fingerprint": fingerprint,
    "arguments": arguments,
}
encoded = (json.dumps(intent, indent=2, sort_keys=True) + "\n").encode()

if created:
    broadcast_dir.mkdir(mode=0o700)
    os.chmod(broadcast_dir, 0o700)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(intent_path, flags, 0o600)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb", closefd=False) as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(fd)
    operation_fd = os.open(operation_dir, os.O_RDONLY)
    try:
        os.fsync(operation_fd)
    finally:
        os.close(operation_fd)
    root_fd = os.open(root, os.O_RDONLY)
    try:
        os.fsync(root_fd)
    finally:
        os.close(root_fd)
    print("fresh")
else:
    try:
        intent_info = intent_path.lstat()
        require_private_dir(broadcast_dir)
    except FileNotFoundError as exc:
        raise SystemExit(
            f"ambiguous via-Gateway repair state for {operation_key}; refusing a fresh replay"
        ) from exc
    if (
        stat.S_ISLNK(intent_info.st_mode)
        or not stat.S_ISREG(intent_info.st_mode)
        or intent_info.st_nlink != 1
        or intent_info.st_uid != os.geteuid()
        or stat.S_IMODE(intent_info.st_mode) != 0o600
    ):
        raise SystemExit(f"unsafe via-Gateway repair intent: {intent_path}")
    if json.loads(intent_path.read_text(encoding="utf-8")) != intent:
        raise SystemExit(
            f"via-Gateway repair intent changed for {operation_key}; refusing replay"
        )
    print("resume")
PY
)" || return $?

  GATEWAY_ADMIN_REPAIR_DIR="${state_dir}/via-gateway-repairs/${operation_key}"
  case "${mode}" in
  fresh) GATEWAY_ADMIN_REPAIR_RESUME=false ;;
  resume) GATEWAY_ADMIN_REPAIR_RESUME=true ;;
  *)
    echo "gateway-launch: invalid via-Gateway repair journal mode: ${mode}" >&2
    return 1
    ;;
  esac
}

finish_gateway_admin_repair() {
  local operation_key="${1:?operation key required}" state_dir
  GATEWAY_ADMIN_REPAIR_DIR=""
  GATEWAY_ADMIN_REPAIR_RESUME=false
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  python3 - "${state_dir}" "${operation_key}" <<'PY'
import os
import re
import shutil
import stat
import sys
from pathlib import Path

state_dir = Path(sys.argv[1])
operation_key = sys.argv[2]
if not re.fullmatch(
    r"(?:committer-role|sender-balance)-[1-9][0-9]*-0x[0-9a-f]{40}",
    operation_key,
):
    raise SystemExit(f"invalid via-Gateway repair operation key: {operation_key}")
root = state_dir / "via-gateway-repairs"
operation_dir = root / operation_key
state_info = state_dir.lstat()
if stat.S_ISLNK(state_info.st_mode) or not stat.S_ISDIR(state_info.st_mode):
    raise SystemExit(f"unsafe via-Gateway repair cleanup path: {state_dir}")
if state_info.st_uid != os.geteuid() or stat.S_IMODE(state_info.st_mode) & 0o077:
    raise SystemExit(f"unsafe via-Gateway repair cleanup ownership/mode: {state_dir}")
if not root.exists() and not root.is_symlink():
    raise SystemExit(0)
root_info = root.lstat()
if stat.S_ISLNK(root_info.st_mode) or not stat.S_ISDIR(root_info.st_mode):
    raise SystemExit(f"unsafe via-Gateway repair cleanup path: {root}")
if root_info.st_uid != os.geteuid() or stat.S_IMODE(root_info.st_mode) & 0o077:
    raise SystemExit(f"unsafe via-Gateway repair cleanup ownership/mode: {root}")
if not operation_dir.exists() and not operation_dir.is_symlink():
    raise SystemExit(0)
operation_info = operation_dir.lstat()
if stat.S_ISLNK(operation_info.st_mode) or not stat.S_ISDIR(operation_info.st_mode):
    raise SystemExit(f"unsafe via-Gateway repair cleanup path: {operation_dir}")
if operation_info.st_uid != os.geteuid() or stat.S_IMODE(operation_info.st_mode) & 0o077:
    raise SystemExit(f"unsafe via-Gateway repair cleanup ownership/mode: {operation_dir}")
shutil.rmtree(operation_dir)
root_fd = os.open(root, os.O_RDONLY)
try:
    os.fsync(root_fd)
finally:
    os.close(root_fd)
PY
}

run_gateway_admin_repair_forge() {
  (
    umask 077
    export FOUNDRY_BROADCAST="${GATEWAY_ADMIN_REPAIR_DIR:?repair journal is not initialized}/broadcast"
    cd "${ZKSYNC_ERA_PATH}/contracts/l1-contracts" || exit $?
    if [ "${GATEWAY_ADMIN_REPAIR_RESUME}" = true ]; then
      set -- "$@" --resume
    fi
    exec forge script deploy-scripts/AdminFunctions.s.sol:AdminFunctions \
      "$@" \
      --rpc-url "${L1_RPC_URL}" \
      --broadcast \
      "${GATEWAY_GOVERNOR_FORGE_WALLET_ARGS[@]}" \
      --slow
  )
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
  local wallet_name sender_addr current_balance_wei top_up_wei
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

    top_up_wei="$(python3 - "${current_balance_wei}" "${min_balance_wei}" <<'PY'
import sys

print(int(sys.argv[2], 10) - int(sys.argv[1], 10))
PY
)"

    echo "gateway-launch: Gateway sender balance below minimum for ${wallet_name} (${sender_addr}): current=${current_balance_wei} wei, minimum=${min_balance_wei} wei; funding ${top_up_wei} wei via L1->Gateway admin tx"

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
      "max_l1_gas_price=${GATEWAY_MAX_L1_GAS_PRICE}" \
      "target=$(gl_to_lower "${sender_addr}")" \
      "value_wei=${top_up_wei}" \
      "calldata=0x" \
      "refund_recipient=$(gl_to_lower "${refund_recipient}")" \
      "minimum_balance_wei=${min_balance_wei}" \
      "observed_balance_wei=${current_balance_wei}"

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
  chain_proxy="$(cast call "${bridgehub}" "getZKChain(uint256)(address)" "${chain_id}" --rpc-url "${L1_RPC_URL}" | awk 'NF { print $1; exit }')" || return $?
  [[ "${chain_proxy}" =~ ^0x[0-9a-fA-F]{40}$ ]] || return 1
  [ "$(gl_to_lower "${chain_proxy}")" != "0x0000000000000000000000000000000000000000" ] || return 1
  paused="$(cast call "${chain_proxy}" "depositsPaused()(bool)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
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
current_settlement_layer="$(get_settlement_layer_chain_id "${EDGE_CHAIN_NAME}")"
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
  [ "${current_settlement_layer}" = "${gateway_chain_id}" ] ||
    gl_die "edge settlement layer does not match Gateway"
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
if [ "${current_settlement_layer}" = "${gateway_chain_id}" ] &&
  is_da_pair_set_on_gateway "${EDGE_CHAIN_NAME}" "${GATEWAY_RPC_URL}" &&
  gateway_required_validator_roles_ready "${EDGE_CHAIN_NAME}"; then
  echo "gateway-launch: ${EDGE_CHAIN_NAME} already settles on Gateway chain ${gateway_chain_id} with DA pair and validator roles set; ensuring sender balances and deposits are unpaused"
  ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
  provision_gateway_settlement_fee_payer "${EDGE_CHAIN_NAME}"
  # SYSCOIN: wrapping settlement fees consumes execute-operator native balance.
  ensure_gateway_commit_sender_balance "${EDGE_CHAIN_NAME}"
  ensure_deposits_unpaused "${EDGE_CHAIN_NAME}"
  exit 0
fi

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

finalize_output=""
finalize_output_lc=""
gl_l1_broadcast_preflight
refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"
if ! finalize_output="$(gl_zkstack_pty zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain "${EDGE_CHAIN_NAME}" \
  --gateway-chain-name "${GATEWAY_CHAIN_NAME}" \
  --l1-rpc-url "${L1_RPC_URL}" \
  --gateway-rpc-url "${GATEWAY_RPC_URL}" \
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
