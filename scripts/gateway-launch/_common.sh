# shellcheck shell=bash
# Source-only: shared paths and helpers for gateway-launch/*.sh
# shellcheck disable=SC2034
_gl_actual_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
if [ "${GL_DIR+x}" = x ]; then
  if ! _gl_supplied_dir="$(cd "${GL_DIR}" 2>/dev/null && pwd -P)" ||
    [ "${_gl_supplied_dir}" != "${_gl_actual_dir}" ]; then
    echo "gateway-launch: GL_DIR must resolve to ${_gl_actual_dir}" >&2
    exit 1
  fi
fi
# SYSCOIN: Preserve only the canonical physical spelling after validation. An
# accepted relative path or symlink must not change meaning after a later cd or retarget.
GL_DIR="${_gl_actual_dir}"
readonly GL_DIR
_gl_repo_root="$(cd "${GL_DIR}/../.." && pwd -P)"
if [ -n "${ZKSYNC_OS_SERVER_PATH:-}" ]; then
  if ! _gl_supplied_repo_root="$(cd "${ZKSYNC_OS_SERVER_PATH}" 2>/dev/null && pwd -P)" ||
    [ "${_gl_supplied_repo_root}" != "${_gl_repo_root}" ]; then
    echo "gateway-launch: ZKSYNC_OS_SERVER_PATH must resolve to ${_gl_repo_root}" >&2
    exit 1
  fi
fi
# SYSCOIN: This checkout contains the pending-fixture marker and the attested
# patch helpers. Do not let callers redirect that security trust root later.
ZKSYNC_OS_SERVER_PATH="${_gl_repo_root}"
readonly ZKSYNC_OS_SERVER_PATH
unset _gl_actual_dir _gl_supplied_dir _gl_repo_root _gl_supplied_repo_root

# Ensure required CLI tooling is discoverable in non-interactive shells.
for _tool_dir in "${HOME}/.foundry/bin" "${HOME}/.cargo/bin"; do
  if [ -d "${_tool_dir}" ] && [[ ":${PATH}:" != *":${_tool_dir}:"* ]]; then
    PATH="${_tool_dir}:${PATH}"
  fi
done
export PATH
# SYSCOIN: Bind every launcher child and standalone helper to the reviewed
# Foundry profile. A caller-supplied profile can change bytecode metadata and
# CREATE2 identities across later zkstack/Forge invocations.
FOUNDRY_PROFILE=default
export FOUNDRY_PROFILE
: "${PROVER_MODE:=gpu}"
export PROVER_MODE

gl_die() {
  echo "gateway-launch: $*" >&2
  exit 1
}

gl_to_lower() {
  printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]'
}

gl_l1_network_requires_external_signer() {
  case "$(gl_to_lower "${L1_NETWORK:-}")" in
  tanenbaum | mainnet) return 0 ;;
  *) return 1 ;;
  esac
}

gl_allow_insecure_private_key_argv() {
  [ "$(gl_to_lower "${GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV:-false}")" = "true" ]
}

gl_validate_l1_signer_policy() {
  if ! gl_l1_network_requires_external_signer || gl_allow_insecure_private_key_argv; then
    return 0
  fi

  local funder_signer deployer_signer governor_signer
  funder_signer="$(gl_to_lower "${FUNDER_SIGNER:-account}")"
  deployer_signer="$(gl_to_lower "${DEPLOYER_SIGNER:-${FUNDER_SIGNER:-account}}")"
  governor_signer="$(gl_to_lower "${EDGE_GATEWAY_GOVERNOR_SIGNER:-${FUNDER_SIGNER:-account}}")"

  if [ "${funder_signer}" = "private-key" ]; then
    gl_die "FUNDER_SIGNER=private-key is not allowed on ${L1_NETWORK}; use account, keystore, hardware wallet, or KMS signing"
  fi
  if [ "${deployer_signer}" = "private-key" ]; then
    gl_die "DEPLOYER_SIGNER=private-key is not allowed on ${L1_NETWORK}; use account, keystore, hardware wallet, or KMS signing"
  fi
  if [ "${governor_signer}" = "private-key" ]; then
    gl_die "EDGE_GATEWAY_GOVERNOR_SIGNER=private-key is not allowed on ${L1_NETWORK}; use account, keystore, hardware wallet, or KMS signing"
  fi
  if [ -n "${FUNDER_PRIVATE_KEY:-}" ]; then
    gl_die "FUNDER_PRIVATE_KEY is not accepted on ${L1_NETWORK}; use FUNDER_SIGNER with account, keystore, hardware wallet, or KMS signing"
  fi
  if [ -n "${DEPLOYER_PRIVATE_KEY:-}" ]; then
    gl_die "DEPLOYER_PRIVATE_KEY is not accepted on ${L1_NETWORK}; use DEPLOYER_SIGNER with account, keystore, hardware wallet, or KMS signing"
  fi
}

gl_validate_prover_mode() {
  local prover_mode_lc
  prover_mode_lc="$(gl_to_lower "${PROVER_MODE}")"
  case "${prover_mode_lc}" in
  gpu | no-proofs) ;;
  *)
    gl_die "invalid PROVER_MODE='${PROVER_MODE}' (expected: gpu | no-proofs)"
    ;;
  esac
  PROVER_MODE="${prover_mode_lc}"
  export PROVER_MODE
}

# SYSCOIN: Direct deployment helpers must not infer a production profile from
# only one of these values; the pair selects constructor and signer policy.
gl_validate_l1_network_pair() {
  L1_NETWORK="$(gl_to_lower "${L1_NETWORK:-}")"
  case "${L1_NETWORK}:${L1_CHAIN_ID:-}" in
  localhost:31337 | tanenbaum:5700 | mainnet:57) ;;
  *) gl_die "unsupported L1_NETWORK/L1_CHAIN_ID pair: ${L1_NETWORK:-<unset>}/${L1_CHAIN_ID:-<unset>}" ;;
  esac
  export L1_NETWORK
}

# SYSCOIN: Canonical deployments never use the upstream dummy MessageRoot, and
# test-verifier selection is durable on-chain state. Normalize both before the
# checkpoint fingerprint or any deployment command can consume them.
gl_normalize_canonical_deployment_inputs() {
  local mock_verifier dummy_message_root root_mode gateway_mode edge_mode gateway_commit_mode l1_network
  mock_verifier="$(gl_to_lower "${SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER:-false}")"
  dummy_message_root="$(gl_to_lower "${USE_DUMMY_MESSAGE_ROOT:-false}")"
  root_mode="$(gl_to_lower "${PROVER_MODE:-gpu}")"
  gateway_mode="$(gl_to_lower "${GATEWAY_PROVER_MODE:-${PROVER_MODE:-gpu}}")"
  edge_mode="$(gl_to_lower "${EDGE_PROVER_MODE:-${PROVER_MODE:-gpu}}")"
  gateway_commit_mode="$(gl_to_lower "${GATEWAY_COMMIT_MODE:-rollup}")"
  l1_network="$(gl_to_lower "${L1_NETWORK:-}")"
  case "${mock_verifier}" in
  true | false) ;;
  *) gl_die "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER must be true or false" ;;
  esac
  case "${dummy_message_root}" in
  false) ;;
  true) gl_die "USE_DUMMY_MESSAGE_ROOT=true is forbidden for canonical Syscoin deployments" ;;
  *) gl_die "USE_DUMMY_MESSAGE_ROOT must be true or false" ;;
  esac
  case "${root_mode}" in
  gpu | no-proofs) ;;
  *) gl_die "PROVER_MODE must be gpu or no-proofs" ;;
  esac
  case "${gateway_mode}" in
  gpu | no-proofs) ;;
  *) gl_die "GATEWAY_PROVER_MODE must be gpu or no-proofs" ;;
  esac
  case "${edge_mode}" in
  gpu | no-proofs) ;;
  *) gl_die "EDGE_PROVER_MODE must be gpu or no-proofs" ;;
  esac
  case "${gateway_commit_mode}" in
  rollup) ;;
  *) gl_die "GATEWAY_COMMIT_MODE must be rollup for compact zkOS DA" ;;
  esac
  [[ "${GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE:-4}" =~ ^0*4$ ]] ||
    gl_die "GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE must be 4 (BlobsZKsyncOS)"
  [ "${GATEWAY_L2_DA_COMMITMENT_SCHEME:-BlobsZKsyncOS}" = "BlobsZKsyncOS" ] ||
    gl_die "GATEWAY_L2_DA_COMMITMENT_SCHEME must be BlobsZKsyncOS"
  [ "${EDGE_GATEWAY_COMMITTER_WALLET_NAME:-blob_operator}" = "blob_operator" ] ||
    gl_die "EDGE_GATEWAY_COMMITTER_WALLET_NAME must be blob_operator"
  if [ "${mock_verifier}" = "true" ]; then
    [ "${l1_network}" != "mainnet" ] ||
      gl_die "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=true is forbidden on mainnet"
    [ "${root_mode}:${gateway_mode}:${edge_mode}" = "no-proofs:no-proofs:no-proofs" ] ||
      gl_die "the mock verifier requires PROVER_MODE, GATEWAY_PROVER_MODE, and EDGE_PROVER_MODE all set to no-proofs"
  elif [ "${root_mode}:${gateway_mode}:${edge_mode}" != "gpu:gpu:gpu" ]; then
    gl_die "the production verifier requires PROVER_MODE, GATEWAY_PROVER_MODE, and EDGE_PROVER_MODE all set to gpu"
  fi
  case "${l1_network}" in
  tanenbaum | mainnet)
    # SYSCOIN: A whitespace-only value is not an explicit deployment identity.
    [[ "${GATEWAY_CREATE2_FACTORY_SALT:-}" =~ [^[:space:]] ]] ||
      gl_die "GATEWAY_CREATE2_FACTORY_SALT is required on ${l1_network}"
    if [ -n "${ZKSYS_ZK_TOKEN_ASSET_ID:-}" ] || [ -n "${ZK_TOKEN_ASSET_ID:-}" ]; then
      gl_die "ZKSYS_ZK_TOKEN_ASSET_ID/ZK_TOKEN_ASSET_ID are derived for canonical Syscoin deployments"
    fi
    ;;
  esac
  export SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER="${mock_verifier}"
  export USE_DUMMY_MESSAGE_ROOT=false
  export PROVER_MODE="${root_mode}"
  export GATEWAY_PROVER_MODE="${gateway_mode}"
  export EDGE_PROVER_MODE="${edge_mode}"
  export GATEWAY_COMMIT_MODE="${gateway_commit_mode}"
  export GATEWAY_L2_DA_COMMITMENT_SCHEME=BlobsZKsyncOS
  export GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE=4
  export EDGE_GATEWAY_COMMITTER_WALLET_NAME=blob_operator
}

# SYSCOIN: The pending V8 key does not authorize the canonical fixture, but an
# exact no-proofs deployment may materialize the reviewed source pair in order
# to reproduce and authenticate the Gateway identity. Keep this gate identical
# in strength to the Era-contract source-materialization exception.
gl_pending_v8_mock_launch_enabled() {
  local gateway_mode edge_mode
  [ "${PROTOCOL_VERSION:-}" = "v32.0" ] || return 1
  [ "${PROVER_MODE:-}" = "no-proofs" ] || return 1
  [ "${SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER:-}" = "true" ] || return 1
  gateway_mode="${GATEWAY_PROVER_MODE:-${PROVER_MODE:-}}"
  edge_mode="${EDGE_PROVER_MODE:-${PROVER_MODE:-}}"
  [ "${gateway_mode}" = "no-proofs" ] || return 1
  [ "${edge_mode}" = "no-proofs" ] || return 1
  case "${L1_NETWORK:-}:${L1_CHAIN_ID:-}" in
  localhost:31337 | tanenbaum:5700) return 0 ;;
  *) return 1 ;;
  esac
}

gl_pending_v8_mock_zkstack_cli_sha() {
  printf '%s\n' "d1f681c395a5b40fd4cfa591dea8ac3d3f80ebdc"
}

gl_pending_v8_mock_contracts_sha() {
  printf '%s\n' "8fb7c29a4e3174335c6480b23f57822e054f9d5f"
}

gl_reject_no_proofs_on_mainnet() {
  local l1_network_lc mode_name mode_value
  l1_network_lc="$(gl_to_lower "${L1_NETWORK:-}")"
  [ "${l1_network_lc}" = "mainnet" ] || return 0

  for mode_name in PROVER_MODE GATEWAY_PROVER_MODE EDGE_PROVER_MODE; do
    mode_value="$(gl_to_lower "${!mode_name:-}")"
    if [ "${mode_value}" = "no-proofs" ]; then
      gl_die "${mode_name}=no-proofs is not allowed for mainnet deployments"
    fi
  done
}

gl_require() {
  local n="$1"
  [ -n "${!n:-}" ] || gl_die "unset required env: $n"
  # SYSCOIN: Relative Era paths can change meaning after launcher helpers cd.
  if [ "${n}" = "ZKSYNC_ERA_PATH" ] && [[ "${!n}" != /* ]]; then
    gl_die "ZKSYNC_ERA_PATH must be absolute"
  fi
}

gl_prepare_wallet_file_for_in_file() {
  local wallet_path="$1"

  # SYSCOIN: wallet files carry deployer/governor/operator private keys. Do not
  # trust predictable in-file wallet paths unless the file is owned by this user,
  # is a regular non-symlink file, and cannot be modified by group/other users.
  python3 - "${wallet_path}" <<'PY' || gl_die "unsafe wallet file: ${wallet_path}"
import os
import stat
import sys

path = sys.argv[1]
try:
    st = os.lstat(path)
except FileNotFoundError:
    raise SystemExit(f"wallet file does not exist: {path}")

if stat.S_ISLNK(st.st_mode):
    raise SystemExit(f"wallet file must not be a symlink: {path}")
if not stat.S_ISREG(st.st_mode):
    raise SystemExit(f"wallet file must be a regular file: {path}")
if st.st_uid != os.geteuid():
    raise SystemExit(f"wallet file must be owned by the launching user: {path}")

mode = stat.S_IMODE(st.st_mode)
if mode & 0o022:
    raise SystemExit(f"wallet file must not be writable by group/other users: {path}")
if mode & 0o077:
    os.chmod(path, 0o600)
PY
}

# SYSCOIN: External signer material must remain private on shared hosts. Empty
# password files are valid, but the file identity and permissions are not optional.
gl_validate_secret_file() {
  local secret_path="${1:?secret path required}" label="${2:?secret label required}"
  if ! SECRET_PATH="${secret_path}" SECRET_LABEL="${label}" python3 - <<'PY'
import os
import stat

path = os.environ["SECRET_PATH"]
label = os.environ["SECRET_LABEL"]
try:
    info = os.lstat(path)
except FileNotFoundError:
    raise SystemExit(f"{label} does not exist: {path}")
if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
    raise SystemExit(f"{label} must be a regular non-symlink file: {path}")
if info.st_uid != os.geteuid():
    raise SystemExit(f"{label} must be owned by the launching user: {path}")
if stat.S_IMODE(info.st_mode) & 0o077:
    raise SystemExit(f"{label} must not be accessible by group/other users: {path}")
PY
  then
    gl_die "unsafe ${label}: ${secret_path}"
  fi
}

# SYSCOIN: Foundry's --account mode resolves a named encrypted keystore from
# its account directory. Authenticate that exact file before handing the name
# to cast/forge so a permissive or symlinked account cannot bypass the explicit
# keystore checks above.
gl_validate_foundry_account_keystore() {
  local account_name="${1:?account name required}" label="${2:?account label required}"
  local keystore_root keystore_path
  [[ "${account_name}" != "." && "${account_name}" != ".." &&
    "${account_name}" != */* ]] ||
    gl_die "${label} must be a single Foundry account name"
  keystore_root="${FOUNDRY_KEYSTORE_DIR:-${HOME}/.foundry/keystores}"
  keystore_path="${keystore_root}/${account_name}"
  gl_validate_secret_file "${keystore_path}" "${label} keystore"
}

gl_secure_generated_wallet_file() {
  local wallet_path="$1"

  # SYSCOIN: zkstack creates generated wallets.yaml files using the process
  # umask. Harden those generated private-key files before later validation.
  python3 - "${wallet_path}" <<'PY' || gl_die "failed to secure generated wallet file: ${wallet_path}"
import os
import stat
import sys

path = sys.argv[1]
try:
    st = os.lstat(path)
except FileNotFoundError:
    raise SystemExit(f"generated wallet file does not exist: {path}")

if stat.S_ISLNK(st.st_mode):
    raise SystemExit(f"generated wallet file must not be a symlink: {path}")
if not stat.S_ISREG(st.st_mode):
    raise SystemExit(f"generated wallet file must be a regular file: {path}")
if st.st_uid != os.geteuid():
    raise SystemExit(f"generated wallet file must be owned by the launching user: {path}")
os.chmod(path, 0o600)
PY
}

gl_wallet_creation_for_path() {
  local wallet_path="$1"
  if [ -e "${wallet_path}" ] || [ -L "${wallet_path}" ]; then
    gl_prepare_wallet_file_for_in_file "${wallet_path}"
    printf 'in-file\n'
  else
    printf 'random\n'
  fi
}

gl_persist_wallet_file() {
  local source_path="$1"
  local dest_path="$2"

  [ -f "${source_path}" ] || gl_die "missing generated wallets file: ${source_path}"

  # SYSCOIN: persist generated private-key material with exclusive 0600 creation
  # so a pre-existing path or symlink cannot be overwritten or followed.
  python3 - "${source_path}" "${dest_path}" <<'PY' || gl_die "failed to persist wallets to ${dest_path}"
import os
import shutil
import sys

source_path = sys.argv[1]
dest_path = sys.argv[2]
parent = os.path.dirname(dest_path) or "."
os.makedirs(parent, exist_ok=True)

flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
fd = None
created = False
try:
    fd = os.open(dest_path, flags, 0o600)
    created = True
    with os.fdopen(fd, "wb") as dst:
        fd = None
        with open(source_path, "rb") as src:
            shutil.copyfileobj(src, dst)
    os.chmod(dest_path, 0o600)
except BaseException:
    if fd is not None:
        os.close(fd)
    if created:
        try:
            os.unlink(dest_path)
        except FileNotFoundError:
            pass
    raise
PY
}

gl_export_foundry_evm_version() {
  : "${FOUNDRY_EVM_VERSION:=cancun}"
  export FOUNDRY_EVM_VERSION
}

# SYSCOIN: Canonicalize the fee before both checkpointing and Gateway
# conversion so a resumed launch cannot silently retain a different fee.
gl_effective_gateway_settlement_fee() {
  python3 - <<'PY'
import os
from decimal import Decimal, ROUND_CEILING, getcontext

raw_fee = os.environ.get("GATEWAY_SETTLEMENT_FEE", "")
if raw_fee:
    raw_fee = raw_fee.strip()
    fee = int(raw_fee, 16) if raw_fee.lower().startswith("0x") else int(raw_fee, 10)
    if fee < 0 or fee >= 1 << 256:
        raise SystemExit("GATEWAY_SETTLEMENT_FEE must fit uint256")
    print(fee)
    raise SystemExit(0)

getcontext().prec = 80
target_raw = os.environ.get("GATEWAY_INTEROP_FEE_USD")
if not target_raw:
    target_raw = os.environ.get("INTEROP_FEE_USD") or "0.15"
native_raw = os.environ.get("NATIVE_TOKEN_PRICE_USD") or "0.01"
decimals_raw = os.environ.get("GATEWAY_INTEROP_FEE_TOKEN_DECIMALS", "18")

target_usd = Decimal(target_raw)
native_price_usd = Decimal(native_raw)
decimals = int(decimals_raw)
if not target_usd.is_finite() or target_usd < 0:
    raise SystemExit("GATEWAY_INTEROP_FEE_USD must be non-negative")
if not native_price_usd.is_finite() or native_price_usd <= 0:
    raise SystemExit("NATIVE_TOKEN_PRICE_USD must be positive")
if decimals < 0 or decimals > 255:
    raise SystemExit("GATEWAY_INTEROP_FEE_TOKEN_DECIMALS must fit uint8")
if os.environ.get("L1_NETWORK", "").strip().lower() in {"tanenbaum", "mainnet"} and decimals != 18:
    raise SystemExit("canonical Syscoin Gateway settlement fees require 18 token decimals")

fee = (target_usd / native_price_usd * (Decimal(10) ** decimals)).to_integral_value(
    rounding=ROUND_CEILING
)
if fee >= 1 << 256:
    raise SystemExit("derived GATEWAY_SETTLEMENT_FEE must fit uint256")
print(int(fee))
PY
}

gl_l1_chain_id_from_rpc() {
  gl_require L1_RPC_URL
  python3 - "${L1_RPC_URL}" <<'PY'
import json
import sys
import urllib.request

rpc_url = sys.argv[1]
payload = json.dumps(
    {"jsonrpc": "2.0", "method": "eth_chainId", "params": [], "id": 1}
).encode("utf-8")
req = urllib.request.Request(
    rpc_url,
    data=payload,
    headers={
        "Content-Type": "application/json",
        "User-Agent": "zksync-os-server-gateway-launch/1.0",
    },
    method="POST",
)
with urllib.request.urlopen(req, timeout=3) as resp:
    body = resp.read().decode("utf-8")
obj = json.loads(body)
result = obj.get("result")
if not isinstance(result, str) or not result.startswith("0x"):
    raise SystemExit(1)
print(int(result, 16))
PY
}

gl_assert_l1_chain_id_matches_rpc() {
  gl_require L1_RPC_URL
  gl_require L1_CHAIN_ID

  local rpc_chain_id
  if ! rpc_chain_id="$(gl_l1_chain_id_from_rpc 2>/dev/null)"; then
    gl_die "failed to read chain id from L1 RPC ${L1_RPC_URL}"
  fi

  if [ -z "${rpc_chain_id}" ]; then
    gl_die "empty chain id from L1 RPC ${L1_RPC_URL}"
  fi

  if [ "${rpc_chain_id}" != "${L1_CHAIN_ID}" ]; then
    gl_die "L1 chain-id mismatch: rpc(${L1_RPC_URL})=${rpc_chain_id}, expected L1_CHAIN_ID=${L1_CHAIN_ID}, FOUNDRY_CHAIN_ID=${FOUNDRY_CHAIN_ID:-<unset>}"
  fi
}

gl_assert_no_chain_id_override_conflicts() {
  gl_require L1_CHAIN_ID
  local expected var_name var_value
  expected="${L1_CHAIN_ID}"

  for var_name in FOUNDRY_CHAIN_ID ETH_CHAIN_ID CHAIN_ID DAPP_CHAIN_ID; do
    var_value="${!var_name:-}"
    if [ -n "${var_value}" ] && [ "${var_value}" != "${expected}" ]; then
      gl_die "conflicting chain-id override env ${var_name}=${var_value} (expected ${expected})"
    fi
  done
}

gl_l1_broadcast_preflight() {
  gl_assert_l1_chain_id_matches_rpc
  gl_assert_no_chain_id_override_conflicts
}

gl_sha_from_versions() {
  gl_require PROTOCOL_VERSION
  local key="$1"
  # SYSCOIN: Refuse to materialize the canonical lane from a placeholder
  # fixture. The sole exception resolves only the exact reviewed source pair
  # for a fake-prover launch; it does not make the absent fixture canonical.
  local pending_marker="${ZKSYNC_OS_SERVER_PATH}/local-chains/${PROTOCOL_VERSION}/CANONICAL_V8_REGENERATION_REQUIRED"
  if [ -f "$pending_marker" ]; then
    gl_pending_v8_mock_launch_enabled || \
      gl_die "local-chain fixture is blocked pending canonical v32.0/V8 regeneration: ${pending_marker}"
    case "$key" in
    era-contracts) gl_pending_v8_mock_contracts_sha ;;
    zkstack-cli) gl_pending_v8_mock_zkstack_cli_sha ;;
    *) gl_die "pending-V8 mock launch has no reviewed source pin for ${key}" ;;
    esac
    return 0
  fi
  local vf="${ZKSYNC_OS_SERVER_PATH}/local-chains/${PROTOCOL_VERSION}/versions.yaml"
  [ -f "$vf" ] || gl_die "missing ${vf}"
  VERSIONS_YAML="$vf" VERSIONS_KEY="$key" python3 - <<'PY'
import os, re

text = open(os.environ["VERSIONS_YAML"], "r", encoding="utf-8").read()
key = re.escape(os.environ["VERSIONS_KEY"])
m = re.search(
    rf"{key}:\s*(?:\n\s*#.*)*\n\s*sha:\s*\"([0-9a-f]{{40}})\"",
    text,
)
if not m:
    raise SystemExit(f"{os.environ['VERSIONS_KEY']} sha not found in versions.yaml")
print(m.group(1))
PY
}

gl_contracts_sha_from_versions() {
  gl_sha_from_versions "era-contracts"
}

gl_zkstack_cli_sha_from_versions() {
  gl_sha_from_versions "zkstack-cli"
}

# SYSCOIN: Resolve and authenticate source pins even when callers pre-populate
# REQUIRED_* variables. This closes the lazy parameter-expansion path that
# otherwise bypasses the pending-fixture marker entirely.
gl_resolve_required_source_pins() {
  gl_require PROTOCOL_VERSION
  local expected_contracts expected_zkstack
  expected_contracts="$(gl_contracts_sha_from_versions)" || \
    gl_die "failed to resolve the reviewed Era-contracts source pin"
  expected_zkstack="$(gl_zkstack_cli_sha_from_versions)" || \
    gl_die "failed to resolve the reviewed zkstack source pin"

  if [ -n "${REQUIRED_CONTRACTS_SHA:-}" ] && [ "${REQUIRED_CONTRACTS_SHA}" != "${expected_contracts}" ]; then
    gl_die "REQUIRED_CONTRACTS_SHA=${REQUIRED_CONTRACTS_SHA} does not match reviewed source pin ${expected_contracts}"
  fi
  if [ -n "${REQUIRED_ZKSTACK_CLI_SHA:-}" ] && [ "${REQUIRED_ZKSTACK_CLI_SHA}" != "${expected_zkstack}" ]; then
    gl_die "REQUIRED_ZKSTACK_CLI_SHA=${REQUIRED_ZKSTACK_CLI_SHA} does not match reviewed source pin ${expected_zkstack}"
  fi

  export REQUIRED_CONTRACTS_SHA="${expected_contracts}"
  export REQUIRED_ZKSTACK_CLI_SHA="${expected_zkstack}"
}

gl_syscoin_edge_da_commit_target_from_gateway_config() {
  gl_require GATEWAY_DIR
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  GATEWAY_CONFIG="${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" python3 - <<'PY'
import os
import re
from pathlib import Path

import yaml

path = Path(os.environ["GATEWAY_CONFIG"])
if not path.exists():
    raise SystemExit(f"missing Gateway config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid Gateway config: {path}")
addr = data.get("validator_timelock_addr")
if isinstance(addr, int):
    if addr <= 0 or addr >= 1 << 160:
        raise SystemExit(f"invalid validator_timelock_addr in {path}: {addr}")
    addr = "0x" + format(addr, "040x")
if not isinstance(addr, str) or not addr.strip():
    raise SystemExit(f"missing validator_timelock_addr in {path}")
addr = addr.strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", addr):
    raise SystemExit(f"invalid validator_timelock_addr in {path}: {addr}")
if addr == "0x" + "0" * 40:
    raise SystemExit(f"validator_timelock_addr must be nonzero in {path}")
print(addr)
PY
}

gl_normalize_syscoin_edge_da_commit_target() {
  local target="${1:?target required}"
  TARGET="${target}" python3 - <<'PY'
import os

addr = os.environ["TARGET"].strip().lower()
if not addr.startswith("0x") or len(addr) != 42:
    raise SystemExit("SYSCOIN_EDGE_DA_COMMIT_TARGET must be a 20-byte hex address")
if any(c not in "0123456789abcdef" for c in addr[2:]):
    raise SystemExit("SYSCOIN_EDGE_DA_COMMIT_TARGET must be a 20-byte hex address")
if addr == "0x" + "0" * 40:
    raise SystemExit("SYSCOIN_EDGE_DA_COMMIT_TARGET must be nonzero")
print(addr)
PY
}

gl_export_syscoin_edge_da_commit_target_from_gateway_config() {
  local expected gateway_chain_name gateway_config var_name var_value var_value_lc
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  gateway_config="${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml"
  if [ -f "${gateway_config}" ]; then
    expected="$(gl_syscoin_edge_da_commit_target_from_gateway_config)"
  else
    expected=""
    for var_name in SYSCOIN_EDGE_DA_COMMIT_TARGET ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET; do
      var_value="${!var_name:-}"
      if [ -n "${var_value}" ]; then
        expected="$(gl_normalize_syscoin_edge_da_commit_target "${var_value}")"
        break
      fi
    done
    [ -n "${expected}" ] ||
      gl_die "missing Gateway config: ${gateway_config}; set SYSCOIN_EDGE_DA_COMMIT_TARGET for workspace-only launches"
  fi
  for var_name in SYSCOIN_EDGE_DA_COMMIT_TARGET ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET; do
    var_value="${!var_name:-}"
    if [ -n "${var_value}" ]; then
      var_value_lc="$(gl_normalize_syscoin_edge_da_commit_target "${var_value}")"
      [ "${var_value_lc}" = "${expected}" ] ||
        gl_die "${var_name}=${var_value} does not match Gateway validator_timelock_addr=${expected}"
    fi
  done
  export SYSCOIN_EDGE_DA_COMMIT_TARGET="${expected}"
}

gl_published_gateway_commit_target() {
  printf '%s\n' "0xca38dbb6ea5f740cc8252f1450def4dcede94478"
}

gl_published_gateway_relay() {
  printf '%s\n' "0x758b06cda80bdd016f79afd0df1a984039067a21"
}

gl_gateway_generated_rpc_url() {
  gl_require GATEWAY_DIR
  local gateway_chain_name config_path address port
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  config_path="${GATEWAY_DIR}/os-server-configs/${gateway_chain_name}/config.yaml"
  [ -f "${config_path}" ] && [ ! -L "${config_path}" ] || \
    gl_die "missing or unsafe Gateway runtime config: ${config_path}"
  address="$(awk '$0 == "rpc:" { if (getline > 0 && $1 == "address:") print $2; exit }' "${config_path}")"
  [[ "${address}" =~ ^(0\.0\.0\.0|127\.0\.0\.1|localhost):([1-9][0-9]{0,4})$ ]] || \
    gl_die "invalid rpc.address in Gateway runtime config ${config_path}: ${address}"
  port="${BASH_REMATCH[2]}"
  [ "${port}" -le 65535 ] || gl_die "invalid Gateway RPC port in ${config_path}: ${port}"
  printf 'http://127.0.0.1:%s\n' "${port}"
}

gl_gateway_runtime_rpc_url() {
  printf '%s\n' "${GATEWAY_RPC_URL:-http://127.0.0.1:${GATEWAY_OS_RPC_PORT:-3052}}"
}

gl_chain_id_from_config() {
  gl_require GATEWAY_DIR
  local chain_name="${1:?chain name required}" label="${2:?chain label required}"
  CHAIN_CONFIG="${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" \
  CHAIN_LABEL="${label}" python3 - <<'PY'
import os
from pathlib import Path

import yaml

label = os.environ["CHAIN_LABEL"]
path = Path(os.environ["CHAIN_CONFIG"])
if not path.is_file():
    raise SystemExit(f"missing {label} chain config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
chain_id = data.get("chain_id") if isinstance(data, dict) else None
if isinstance(chain_id, str):
    try:
        chain_id = int(chain_id, 16 if chain_id.lower().startswith("0x") else 10)
    except ValueError:
        raise SystemExit(f"invalid {label} chain_id in {path}") from None
if isinstance(chain_id, bool) or not isinstance(chain_id, int) or not 0 < chain_id < 2**256:
    raise SystemExit(f"invalid {label} chain_id in {path}")
print(chain_id)
PY
}

gl_gateway_chain_id_from_config() {
  gl_chain_id_from_config "${GATEWAY_CHAIN_NAME:-gateway}" "Gateway"
}

gl_assert_rpc_chain_id_matches_config() {
  local rpc_url="${1:?RPC URL required}" chain_name="${2:?chain name required}"
  local label="${3:?chain label required}" expected actual
  expected="$(gl_chain_id_from_config "${chain_name}" "${label}")" || return $?
  actual="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast chain-id --rpc-url "${rpc_url}")" ||
    gl_die "failed to read ${label} chain ID from ${rpc_url}"
  actual="$(printf '%s' "${actual}" | tr -d '[:space:]')"
  [[ "${actual}" =~ ^[0-9]+$ ]] ||
    gl_die "invalid ${label} RPC chain ID from ${rpc_url}: ${actual:-<empty>}"
  [ "${actual}" = "${expected}" ] ||
    gl_die "${label} RPC chain ID mismatch: config=${expected} rpc=${actual}"
}

# SYSCOIN: Bind the complete security profile emitted by zkstack before an L1
# deployment/init transaction can consume that local directory.
gl_assert_chain_config_matches_expected() {
  local chain_name="${1:?chain name required}" label="${2:?chain label required}"
  local configured_id="${3:?configured chain ID required}"
  local configured_prover="${4:?configured prover mode required}"
  local configured_commit="${5:?configured commitment mode required}"
  CHAIN_CONFIG="${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" \
  CHAIN_LABEL="${label}" \
  CHAIN_EXPECTED_NAME="${chain_name}" \
  CHAIN_EXPECTED_ID="${configured_id}" \
  CHAIN_EXPECTED_PROVER="${configured_prover}" \
  CHAIN_EXPECTED_COMMIT="${configured_commit}" \
  CHAIN_EXPECTED_L1_NETWORK="${L1_NETWORK:-}" python3 - <<'PY'
import os
from pathlib import Path

import yaml

label = os.environ["CHAIN_LABEL"]
path = Path(os.environ["CHAIN_CONFIG"])
if not path.is_file():
    raise SystemExit(f"missing {label} chain config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid {label} chain config: {path}")

expected_id_raw = os.environ["CHAIN_EXPECTED_ID"].strip()
if not expected_id_raw.isdecimal():
    raise SystemExit(f"{label} chain ID must be an unsigned decimal integer")
expected_id = int(expected_id_raw, 10)
if not 0 < expected_id < 2**32:
    raise SystemExit(f"{label} chain ID must be between 1 and 4294967295")
actual_id = data.get("chain_id")
if isinstance(actual_id, str):
    try:
        actual_id = int(actual_id, 16 if actual_id.lower().startswith("0x") else 10)
    except ValueError:
        actual_id = None
if isinstance(actual_id, bool) or actual_id != expected_id:
    raise SystemExit(
        f"{label} chain_id mismatch: configured={expected_id} persisted={actual_id}"
    )
if data.get("name") != os.environ["CHAIN_EXPECTED_NAME"]:
    raise SystemExit(f"{label} chain name does not match its selected directory")

provers = {"gpu": "Gpu", "no-proofs": "NoProofs"}
expected_prover_raw = os.environ["CHAIN_EXPECTED_PROVER"].strip().lower()
expected_prover = provers.get(expected_prover_raw)
if expected_prover is None:
    raise SystemExit(f"invalid configured {label} prover mode: {expected_prover_raw}")
actual_prover = data.get("prover_version")
if actual_prover != expected_prover:
    raise SystemExit(
        f"{label} prover_version mismatch: configured={expected_prover} "
        f"persisted={actual_prover}"
    )

commitments = {"rollup": "Rollup"}
expected_commit_raw = os.environ["CHAIN_EXPECTED_COMMIT"].strip().lower()
expected_commit = commitments.get(expected_commit_raw)
if expected_commit is None:
    raise SystemExit(f"invalid configured {label} commitment mode: {expected_commit_raw}")
actual_commit = data.get("l1_batch_commit_data_generator_mode")
if actual_commit != expected_commit:
    raise SystemExit(
        f"{label} commitment mode mismatch: configured={expected_commit} "
        f"persisted={actual_commit}"
    )

if data.get("vm_option") != "ZKSyncOsVM":
    raise SystemExit(f"{label} vm_option must be ZKSyncOsVM")
if data.get("evm_emulator") is not False:
    raise SystemExit(f"{label} evm_emulator must be false")
if data.get("legacy_bridge") is True:
    raise SystemExit(f"{label} legacy_bridge must not be enabled")

base_token = data.get("base_token")
if not isinstance(base_token, dict):
    raise SystemExit(f"{label} base_token must be an object")
base_address = base_token.get("address")
if isinstance(base_address, int) and not isinstance(base_address, bool):
    base_address = "0x" + format(base_address, "040x")
if not isinstance(base_address, str):
    raise SystemExit(f"{label} base_token.address is invalid")
base_address = base_address.strip().lower()
if base_address != "0x0000000000000000000000000000000000000001":
    raise SystemExit(f"{label} base_token.address must be the native-token sentinel")
for field in ("nominator", "denominator"):
    value = base_token.get(field)
    if isinstance(value, str) and value.isdecimal():
        value = int(value, 10)
    if isinstance(value, bool) or value != 1:
        raise SystemExit(f"{label} base_token.{field} must equal 1")

expected_network = os.environ["CHAIN_EXPECTED_L1_NETWORK"].strip().lower()
actual_network = data.get("l1_network")
if actual_network is not None and (
    not isinstance(actual_network, str) or actual_network.lower() != expected_network
):
    raise SystemExit(
        f"{label} l1_network mismatch: configured={expected_network} "
        f"persisted={actual_network}"
    )
PY
}

gl_assert_ecosystem_config_matches_expected() {
  ECOSYSTEM_CONFIG="${GATEWAY_DIR}/ZkStack.yaml" \
  EXPECTED_PROVER_MODE="${GATEWAY_PROVER_MODE:-${PROVER_MODE:-gpu}}" \
  EXPECTED_L1_NETWORK="${L1_NETWORK:-}" python3 - <<'PY'
import os
from pathlib import Path

import yaml

path = Path(os.environ["ECOSYSTEM_CONFIG"])
if not path.is_file():
    raise SystemExit(f"missing ecosystem config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid ecosystem config: {path}")
provers = {"gpu": "Gpu", "no-proofs": "NoProofs"}
expected_prover = provers.get(os.environ["EXPECTED_PROVER_MODE"].strip().lower())
if expected_prover is None:
    raise SystemExit("invalid configured Gateway prover mode")
if data.get("prover_version") != expected_prover:
    raise SystemExit(
        f"ecosystem prover_version mismatch: configured={expected_prover} "
        f"persisted={data.get('prover_version')}"
    )
expected_network = os.environ["EXPECTED_L1_NETWORK"].strip().lower()
actual_network = data.get("l1_network")
if not isinstance(actual_network, str) or actual_network.lower() != expected_network:
    raise SystemExit(
        f"ecosystem l1_network mismatch: configured={expected_network} "
        f"persisted={actual_network}"
    )
PY
}

gl_assert_gateway_chain_config_matches_expected() {
  gl_assert_ecosystem_config_matches_expected || return $?
  gl_assert_chain_config_matches_expected \
    "${GATEWAY_CHAIN_NAME:-gateway}" \
    "Gateway" \
    "${GATEWAY_CHAIN_ID:-57001}" \
    "${GATEWAY_PROVER_MODE:-${PROVER_MODE:-gpu}}" \
    "${GATEWAY_COMMIT_MODE:-rollup}"
}

gl_assert_edge_chain_config_matches_expected() {
  local expected
  if [ -n "${EDGE_CHAIN_ID:-}" ]; then
    expected="${EDGE_CHAIN_ID}"
  elif [ "${EDGE_CHAIN_NAME:-zksys}" = "zksys" ]; then
    expected="57057"
  else
    gl_die "EDGE_CHAIN_ID is required for non-default edge ${EDGE_CHAIN_NAME}"
  fi
  gl_assert_chain_config_matches_expected \
    "${EDGE_CHAIN_NAME:-zksys}" \
    "edge" \
    "${expected}" \
    "${EDGE_PROVER_MODE:-${PROVER_MODE:-gpu}}" \
    "rollup"
}

# SYSCOIN: The candidate code/address checks are deployment-agnostic. Pin the
# first launcher-owned Gateway's immutable block 0 so direct additional-edge
# helpers cannot attach to another V32 Gateway with the same chain ID and code.
gl_assert_gateway_genesis_stamp() {
  local gateway_rpc="${1:?Gateway RPC URL required}"
  local chain_id="${2:?Gateway chain ID required}"
  local allow_create="${3:-false}"
  local expected_owner_pid="${4:-${GATEWAY_RUNTIME_OWNER_PID:-}}"
  local block_zero_hash gateway_chain_name stamp_path expected
  case "${allow_create}" in
  true | false) ;;
  *) gl_die "invalid Gateway genesis-stamp policy: ${allow_create}" ;;
  esac
  if [ -n "${expected_owner_pid}" ]; then
    gl_assert_gateway_listener_owned_by_pid "${expected_owner_pid}" "${gateway_rpc}" || return $?
  fi
  block_zero_hash="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast block 0 --field hash --rpc-url "${gateway_rpc}")" || \
    gl_die "failed to read Gateway block-0 hash from ${gateway_rpc}"
  if [ -n "${expected_owner_pid}" ]; then
    # SYSCOIN: Do not persist an RPC result after its launcher-owned listener disappeared.
    gl_assert_gateway_listener_owned_by_pid "${expected_owner_pid}" "${gateway_rpc}" || return $?
  fi
  block_zero_hash="$(gl_to_lower "$(printf '%s' "${block_zero_hash}" | tr -d '[:space:]')")"
  [[ "${block_zero_hash}" =~ ^0x[0-9a-f]{64}$ ]] || \
    gl_die "invalid Gateway block-0 hash from ${gateway_rpc}: ${block_zero_hash}"
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  stamp_path="${GATEWAY_DIR}/.gateway-launch/${gateway_chain_name}-runtime-genesis.v1"
  expected="${chain_id} ${block_zero_hash}"
  if [ -n "${expected_owner_pid}" ]; then
    # SYSCOIN: Reconfirm ownership at the final boundary before persistence.
    gl_assert_gateway_listener_owned_by_pid "${expected_owner_pid}" "${gateway_rpc}" || return $?
  fi
  GATEWAY_GENESIS_STAMP="${stamp_path}" \
  GATEWAY_GENESIS_EXPECTED="${expected}" \
  GATEWAY_GENESIS_ALLOW_CREATE="${allow_create}" python3 - <<'PY'
import os
import stat
from pathlib import Path

path = Path(os.environ["GATEWAY_GENESIS_STAMP"])
expected = os.environ["GATEWAY_GENESIS_EXPECTED"] + "\n"
allow_create = os.environ["GATEWAY_GENESIS_ALLOW_CREATE"] == "true"
if not path.exists() and not path.is_symlink():
    if not allow_create:
        raise SystemExit(f"missing Gateway genesis stamp {path}; run the main launcher first")
    path.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    with os.fdopen(os.open(path, flags, 0o600), "w", encoding="utf-8") as stream:
        stream.write(expected)
        stream.flush()
        os.fsync(stream.fileno())
info = path.lstat()
if path.is_symlink() or not stat.S_ISREG(info.st_mode):
    raise SystemExit(f"Gateway genesis stamp must be a regular non-symlink file: {path}")
if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
    raise SystemExit(f"unsafe Gateway genesis stamp ownership or permissions: {path}")
actual = path.read_text(encoding="utf-8")
if actual != expected:
    raise SystemExit(f"Gateway deployment genesis mismatch in {path}")
PY
}

gl_gateway_relay_from_gateway_config() {
  gl_require GATEWAY_DIR
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  GATEWAY_CONFIG="${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" python3 - <<'PY'
import os
import re
from pathlib import Path

import yaml

path = Path(os.environ["GATEWAY_CONFIG"])
if not path.exists():
    raise SystemExit(f"missing Gateway config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid Gateway config: {path}")
addr = data.get("relayed_sl_da_validator")
if isinstance(addr, int):
    if addr <= 0 or addr >= 1 << 160:
        raise SystemExit(f"invalid relayed_sl_da_validator in {path}: {addr}")
    addr = "0x" + format(addr, "040x")
if not isinstance(addr, str):
    raise SystemExit(f"missing relayed_sl_da_validator in {path}")
addr = addr.strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", addr) or addr == "0x" + "0" * 40:
    raise SystemExit(f"invalid relayed_sl_da_validator in {path}: {addr}")
print(addr)
PY
}

# SYSCOIN: A fresh source-only mock deployment may reach Gateway conversion,
# but it must not create an edge against a deployment identity different from
# the app-bound integration candidate. A mismatch is the expected signal to
# stop, attest the new deployment inputs, and repin through review.
gl_assert_gateway_config_identity() {
  local actual_target expected_target actual_relay expected_relay
  gl_assert_gateway_chain_config_matches_expected || return $?
  actual_target="$(gl_syscoin_edge_da_commit_target_from_gateway_config)" || return $?
  expected_target="$(gl_published_gateway_commit_target)"
  [ "${actual_target}" = "${expected_target}" ] || \
    gl_die "fresh Gateway validator_timelock_addr=${actual_target} differs from app-bound target ${expected_target}; stopped before edge creation for identity repinning/review"

  actual_relay="$(gl_gateway_relay_from_gateway_config)" || return $?
  expected_relay="$(gl_published_gateway_relay)"
  [ "${actual_relay}" = "${expected_relay}" ] || \
    gl_die "fresh Gateway relayed_sl_da_validator=${actual_relay} differs from app-bound relay ${expected_relay}; stopped before edge creation for identity repinning/review"
}

gl_assert_rpc_runtime_identity() {
  local rpc_url="${1:?rpc url required}"
  local address="${2:?address required}"
  local expected_size="${3:?expected size required}"
  local expected_hash="${4:?expected hash required}"
  local label="${5:?label required}"
  local code code_hex actual_size actual_hash

  code="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast code "${address}" --rpc-url "${rpc_url}")" || \
    gl_die "failed to read ${label} runtime at ${address} from ${rpc_url}"
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ "${code#0x}" != "${code}" ] && [ "${code}" != "0x" ] || \
    gl_die "missing ${label} runtime at ${address} on ${rpc_url}"
  code_hex="${code#0x}"
  [ $(( ${#code_hex} % 2 )) -eq 0 ] || gl_die "malformed ${label} runtime at ${address}"
  actual_size=$(( ${#code_hex} / 2 ))
  if [ "${expected_size}" -ne 0 ]; then
    [ "${actual_size}" -eq "${expected_size}" ] || \
      gl_die "${label} runtime size mismatch at ${address}: expected=${expected_size} actual=${actual_size}"
  fi
  actual_hash="$(cast keccak "${code}")"
  actual_hash="$(gl_to_lower "${actual_hash}")"
  expected_hash="$(gl_to_lower "${expected_hash}")"
  [ "${actual_hash}" = "${expected_hash}" ] || \
    gl_die "${label} runtime hash mismatch at ${address}: expected=${expected_hash} actual=${actual_hash}"
}

gl_normalize_gateway_address() {
  local label="${1:?label required}" raw="${2:-}" normalized
  normalized="$(printf '%s\n' "${raw}" | awk 'NF { print tolower($1); exit }')"
  [[ "${normalized}" =~ ^0x[0-9a-f]{40}$ ]] ||
    gl_die "invalid ${label}: ${raw:-<empty>}"
  [ "${normalized}" != "0x0000000000000000000000000000000000000000" ] ||
    gl_die "${label} must be nonzero"
  printf '%s\n' "${normalized}"
}

gl_gateway_wrapped_base_token_from_rpc() {
  local gateway_rpc="${1:?Gateway RPC URL required}"
  local native_token_vault="0x0000000000000000000000000000000000010004"
  local asset_tracker="0x0000000000000000000000000000000000010010"
  local vault_token tracker_token code
  vault_token="$(gl_normalize_gateway_address \
    "Gateway native-token vault WETH_TOKEN" \
    "$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
      cast call "${native_token_vault}" "WETH_TOKEN()(address)" --rpc-url "${gateway_rpc}")")" || return $?
  tracker_token="$(gl_normalize_gateway_address \
    "Gateway asset tracker wrappedZKToken" \
    "$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
      cast call "${asset_tracker}" "wrappedZKToken()(address)" --rpc-url "${gateway_rpc}")")" || return $?
  [ "${vault_token}" = "${tracker_token}" ] ||
    gl_die "Gateway wrapped base-token mismatch: native-token-vault=${vault_token} asset-tracker=${tracker_token}"
  code="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast code "${tracker_token}" --rpc-url "${gateway_rpc}")" ||
    gl_die "failed to read Gateway wrapped base-token runtime at ${tracker_token}"
  code="$(printf '%s' "${code}" | tr -d '[:space:]')"
  [ -n "${code}" ] && [ "${code}" != "0x" ] ||
    gl_die "Gateway wrapped base token has no code at ${tracker_token}"
  printf '%s\n' "${tracker_token}"
}

gl_assert_gateway_wrapped_base_token_pin() {
  local gateway_rpc="${1:?Gateway RPC URL required}" expected actual
  gl_require GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS
  expected="$(gl_normalize_gateway_address \
    "GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS" \
    "${GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS}")" || return $?
  actual="$(gl_gateway_wrapped_base_token_from_rpc "${gateway_rpc}")" || return $?
  [ "${actual}" = "${expected}" ] ||
    gl_die "Gateway wrapped base-token pin mismatch: expected=${expected} rpc=${actual}"
  export GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS="${expected}"
}

# SYSCOIN: RPC contents alone cannot distinguish the node launched by this
# process from an unrelated local listener that wins the startup port race.
gl_assert_gateway_listener_owned_by_pid() {
  local expected_pid="${1:?expected PID required}"
  local gateway_rpc="${2:?Gateway RPC URL required}"
  local port
  [[ "${expected_pid}" =~ ^[1-9][0-9]*$ ]] ||
    gl_die "invalid launcher-owned Gateway PID: ${expected_pid}"
  [[ "${gateway_rpc}" =~ ^http://127\.0\.0\.1:([1-9][0-9]{0,4})$ ]] ||
    gl_die "listener ownership requires a generated loopback Gateway RPC URL: ${gateway_rpc}"
  port="${BASH_REMATCH[1]}"
  [ "${port}" -le 65535 ] || gl_die "invalid Gateway RPC port: ${port}"

  python3 - "${expected_pid}" "${port}" <<'PY'
import os
import re
import subprocess
import sys
from pathlib import Path

pid = int(sys.argv[1])
port = int(sys.argv[2])


def fail(reason):
    raise SystemExit(
        f"Gateway RPC listener on 127.0.0.1:{port} is not exclusively owned "
        f"by launcher PID {pid}: {reason}"
    )


if sys.platform.startswith("linux"):
    fd_dir = Path(f"/proc/{pid}/fd")
    tcp = Path("/proc/net/tcp")
    tcp6 = Path("/proc/net/tcp6")
    if not fd_dir.is_dir() or not tcp.is_file():
        fail("required Linux procfs entries are unavailable")
    tables = [tcp, *([tcp6] if tcp6.is_file() else [])]

    owner_inodes = set()
    try:
        entries = list(fd_dir.iterdir())
    except OSError as error:
        fail(f"cannot inspect process file descriptors ({error})")
    for entry in entries:
        try:
            target = os.readlink(entry)
        except FileNotFoundError:
            continue
        except OSError as error:
            fail(f"cannot inspect process file descriptor {entry.name} ({error})")
        match = re.fullmatch(r"socket:\[(\d+)\]", target)
        if match:
            owner_inodes.add(match.group(1))

    listener_inodes = set()
    for table in tables:
        try:
            lines = table.read_text(encoding="ascii").splitlines()[1:]
        except OSError as error:
            fail(f"cannot inspect {table} ({error})")
        for line in lines:
            fields = line.split()
            if len(fields) < 10:
                fail(f"malformed row in {table}")
            try:
                local_port = int(fields[1].rsplit(":", 1)[1], 16)
            except (IndexError, ValueError):
                fail(f"malformed local address in {table}")
            if fields[3] == "0A" and local_port == port:
                inode = fields[9]
                if not inode.isdecimal():
                    fail(f"malformed listener inode in {table}")
                listener_inodes.add(inode)

    if not listener_inodes:
        fail("no listening socket was found")
    if not listener_inodes.issubset(owner_inodes):
        fail("another process owns the listening socket")
elif sys.platform == "darwin":
    # SYSCOIN: Do not resolve this security check through caller-controlled PATH.
    lsof = Path("/usr/sbin/lsof")
    if not lsof.is_file() or not os.access(lsof, os.X_OK):
        fail("lsof is unavailable")
    result = subprocess.run(
        [
            str(lsof),
            "-nP",
            "-a",
            f"-iTCP:{port}",
            "-sTCP:LISTEN",
            "-Fp",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        fail("lsof could not identify the listening process")
    listener_pids = {
        int(line[1:])
        for line in result.stdout.splitlines()
        if re.fullmatch(r"p[1-9][0-9]*", line)
    }
    if listener_pids != {pid}:
        fail("another process owns the listening socket")
else:
    fail(f"unsupported platform {sys.platform}")
PY
}

gl_assert_gateway_runtime_identity() {
  local expected_owner_pid="${1:-${GATEWAY_RUNTIME_OWNER_PID:-}}"
  local allow_genesis_stamp_creation="${2:-false}"
  local gateway_rpc_override="${3:-}"
  local gateway_rpc target relay factory expected_chain_id actual_chain_id
  case "${allow_genesis_stamp_creation}" in
  true | false) ;;
  *) gl_die "invalid Gateway runtime genesis-stamp policy: ${allow_genesis_stamp_creation}" ;;
  esac
  if [ -n "${expected_owner_pid}" ]; then
    [[ "${expected_owner_pid}" =~ ^[1-9][0-9]*$ ]] && \
      kill -0 "${expected_owner_pid}" 2>/dev/null || \
      gl_die "launcher-owned Gateway PID is not alive: ${expected_owner_pid}"
  elif [ "${allow_genesis_stamp_creation}" = true ]; then
    gl_die "creating the Gateway genesis stamp requires a live launcher-owned PID"
  fi
  gateway_rpc="${gateway_rpc_override:-$(gl_gateway_runtime_rpc_url)}"
  if [ -n "${expected_owner_pid}" ]; then
    gl_assert_gateway_listener_owned_by_pid "${expected_owner_pid}" "${gateway_rpc}" || return $?
  fi
  if [ "${allow_genesis_stamp_creation}" = true ]; then
    [ "${gateway_rpc}" = "$(gl_gateway_generated_rpc_url)" ] || \
      gl_die "launcher-owned Gateway stamp creation must attest its generated local RPC endpoint"
  fi
  gl_assert_gateway_config_identity || return $?
  expected_chain_id="$(gl_gateway_chain_id_from_config)" || return $?
  actual_chain_id="$(env -u FOUNDRY_CHAIN_ID -u ETH_CHAIN_ID -u CHAIN_ID -u DAPP_CHAIN_ID \
    cast chain-id --rpc-url "${gateway_rpc}")" || \
    gl_die "failed to read Gateway chain ID from ${gateway_rpc}"
  [[ "${actual_chain_id}" =~ ^[0-9]+$ ]] || \
    gl_die "invalid Gateway RPC chain ID from ${gateway_rpc}: ${actual_chain_id}"
  [ "${actual_chain_id}" = "${expected_chain_id}" ] || \
    gl_die "Gateway RPC chain ID mismatch: config=${expected_chain_id} rpc=${actual_chain_id}"
  target="$(gl_published_gateway_commit_target)"
  relay="$(gl_published_gateway_relay)"
  factory="0x4e59b44847b379578588920ca78fbf26c0b4956c"

  # SYSCOIN: These hashes and sizes are part of the reviewed V32 application
  # identity. Check all three live postimages before an edge can settle here.
  gl_assert_rpc_runtime_identity \
    "${gateway_rpc}" "${target}" 2840 \
    "0xd98965fa7f49fc4302a2d161454fb0ef619516fbb05a24724e64bb3a3e06e5c4" \
    "Gateway ValidatorTimelock" || return $?
  gl_assert_rpc_runtime_identity \
    "${gateway_rpc}" "${relay}" 0 \
    "0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0" \
    "compact Edge-DA relay" || return $?
  gl_assert_rpc_runtime_identity \
    "${gateway_rpc}" "${factory}" 0 \
    "0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989" \
    "Arachnid CREATE2 factory" || return $?
  if [ -n "${expected_owner_pid}" ]; then
    # SYSCOIN: First-start attestation may persist the immutable genesis stamp.
    # Rebind the RPC socket to the exact launcher process immediately before it.
    gl_assert_gateway_listener_owned_by_pid "${expected_owner_pid}" "${gateway_rpc}" || return $?
  fi
  gl_assert_gateway_genesis_stamp \
    "${gateway_rpc}" "${expected_chain_id}" "${allow_genesis_stamp_creation}" \
    "${expected_owner_pid}" || return $?
}

# SYSCOIN: zkSYS gas-tank address recorded in the edge chain's contracts
# config. Missing/zero is allowed only for first boot; the OS still keeps the
# immutable published address and falls back to native fees until deployment.
gl_zksys_gas_tank_from_edge_config() {
  gl_require GATEWAY_DIR
  local edge_chain_name
  edge_chain_name="${EDGE_CHAIN_NAME:-zksys}"
  EDGE_CONFIG_DIR="${GATEWAY_DIR}/chains/${edge_chain_name}/configs" \
  EDGE_CHAIN_ID_VALUE="${EDGE_CHAIN_ID:-}" \
    python3 - <<'PY'
import os
from pathlib import Path

import yaml

config_dir = Path(os.environ["EDGE_CONFIG_DIR"])
paths = [config_dir / "contracts.yaml"]
chain_id = os.environ.get("EDGE_CHAIN_ID_VALUE", "").strip()
if chain_id:
    paths.append(config_dir / f"contracts_{chain_id}.yaml")
paths.extend(sorted(config_dir.glob("contracts_*.yaml")))
for path in paths:
    if path.exists():
        break
else:
    raise SystemExit(f"missing edge contracts config under {config_dir}")

data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid edge contracts config: {path}")
l2 = data.get("l2")
if not isinstance(l2, dict):
    raise SystemExit(f"missing l2 section in {path}")
addr = l2.get("zksys_gas_tank_addr")
if addr is None:
    print("0x" + "0" * 40)
    raise SystemExit(0)
if isinstance(addr, int):
    addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
if not isinstance(addr, str) or not addr.strip():
    raise SystemExit(f"invalid l2.zksys_gas_tank_addr in {path}")
addr = addr.strip().lower()
if not addr.startswith("0x") or len(addr) != 42:
    raise SystemExit(f"invalid l2.zksys_gas_tank_addr in {path}: {addr}")
if any(c not in "0123456789abcdef" for c in addr[2:]):
    raise SystemExit(f"invalid l2.zksys_gas_tank_addr in {path}: {addr}")
print(addr)
PY
}

gl_normalize_syscoin_gas_tank_address() {
  local target="${1:?target required}"
  TARGET="${target}" python3 - <<'PY'
import os

addr = os.environ["TARGET"].strip().lower()
if not addr.startswith("0x") or len(addr) != 42:
    raise SystemExit("SYSCOIN_GAS_TANK_ADDRESS must be a 20-byte hex address")
if any(c not in "0123456789abcdef" for c in addr[2:]):
    raise SystemExit("SYSCOIN_GAS_TANK_ADDRESS must be a 20-byte hex address")
print(addr)
PY
}

# SYSCOIN: validate the edge chain's gas-tank address against the immutable
# value already bound to the canonical application and VK.
gl_export_syscoin_gas_tank_address_from_edge_config() {
  local auto_require_after_deployment="${1:-false}"
  local expected var_name var_value var_value_lc
  case "${auto_require_after_deployment}" in
  true | false) ;;
  *) gl_die "invalid gas-tank auto-require policy: ${auto_require_after_deployment}" ;;
  esac
  case "${SYSCOIN_REQUIRE_GAS_TANK:-}" in
  "" | 0 | 1) ;;
  *) gl_die "SYSCOIN_REQUIRE_GAS_TANK must be exactly 0 or 1" ;;
  esac
  expected="$(gl_zksys_gas_tank_from_edge_config)"
  if [ "${expected}" = "0x0000000000000000000000000000000000000000" ]; then
    # First boot must use the same immutable canonical source as the published
    # guest even though the contract has not been deployed yet. The OS falls
    # back to native fee payment until zksys-l2-bootstrap records the tank.
    if [ "${SYSCOIN_REQUIRE_GAS_TANK:-0}" = "1" ]; then
      gl_die "SYSCOIN_REQUIRE_GAS_TANK=1 but l2.zksys_gas_tank_addr is missing/zero; deploy the published gas tank with zksys-l2-bootstrap.sh before launch"
    fi
    [ -n "${SYSCOIN_GAS_TANK_ADDRESS:-}" ] ||
      gl_die "missing immutable SYSCOIN_GAS_TANK_ADDRESS during first-boot validation"
    echo "gateway-launch: WARNING: l2.zksys_gas_tank_addr is missing/zero; using the canonical address before its first-boot deployment" >&2
    return 0
  fi
  for var_name in SYSCOIN_GAS_TANK_ADDRESS ZKSYNC_OS_SYSCOIN_GAS_TANK_ADDRESS; do
    var_value="${!var_name:-}"
    if [ -n "${var_value}" ]; then
      var_value_lc="$(gl_normalize_syscoin_gas_tank_address "${var_value}")"
      [ "${var_value_lc}" = "${expected}" ] ||
        gl_die "${var_name}=${var_value} does not match l2.zksys_gas_tank_addr=${expected}"
    fi
  done
  export SYSCOIN_GAS_TANK_ADDRESS="${expected}"
  if [ "${auto_require_after_deployment}" = "true" ]; then
    # SYSCOIN: zksys-l2-bootstrap persists this nonzero address only after
    # attesting the exact runtime, immutable token, and burner role. Promote
    # the canonical main-node launch policy automatically so an operator cannot
    # accidentally leave the first-boot exception enabled in production.
    if [ "${SYSCOIN_REQUIRE_GAS_TANK:-}" = "0" ]; then
      echo "gateway-launch: ignoring SYSCOIN_REQUIRE_GAS_TANK=0 after the canonical gas-tank deployment was persisted" >&2
    fi
    export SYSCOIN_REQUIRE_GAS_TANK=1
  fi
}

gl_assert_contracts_sha() {
  gl_resolve_required_source_pins
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_CONTRACTS_SHA
  local head
  head="$(git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD)"
  [ "$head" = "${REQUIRED_CONTRACTS_SHA}" ] ||
    gl_die "contracts HEAD ${head} != REQUIRED_CONTRACTS_SHA ${REQUIRED_CONTRACTS_SHA}"
}

gl_checkout_contracts_sha() {
  gl_resolve_required_source_pins
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_CONTRACTS_SHA
  git -C "${ZKSYNC_ERA_PATH}" submodule update --init contracts
  git -C "${ZKSYNC_ERA_PATH}/contracts" fetch origin "${REQUIRED_CONTRACTS_SHA}"
  git -C "${ZKSYNC_ERA_PATH}/contracts" checkout "${REQUIRED_CONTRACTS_SHA}"
  git -C "${ZKSYNC_ERA_PATH}/contracts" submodule sync --recursive
  git -C "${ZKSYNC_ERA_PATH}/contracts" submodule update --init --recursive
}

gl_assert_zksync_era_sha() {
  gl_resolve_required_source_pins
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  local head
  head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD)"
  if [ "$head" = "${REQUIRED_ZKSTACK_CLI_SHA}" ]; then
    return 0
  fi
  git -C "${ZKSYNC_ERA_PATH}" merge-base --is-ancestor "${REQUIRED_ZKSTACK_CLI_SHA}" "${head}" ||
    gl_die "zksync-era HEAD ${head} is not based on REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA}"
  local committed_delta
  committed_delta="$(git -C "${ZKSYNC_ERA_PATH}" diff --name-only "${REQUIRED_ZKSTACK_CLI_SHA}..${head}")"
  [ "${committed_delta}" = "contracts" ] ||
    gl_die "zksync-era HEAD ${head} differs from REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA} outside contracts: ${committed_delta}"
}

# SYSCOIN: Nightly toolchain discovery for zkstack_cli. Use an interval-free
# expression because Debian's default mawk does not enable `{n}` regex intervals.
gl_detect_gateway_zkstack_nightly() {
  if command -v rustup >/dev/null 2>&1; then
    rustup toolchain list |
      awk '/^nightly-[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9](-|$)/ {print $1}' |
      sort -V | tail -n 1
  fi
}

# Apply Syscoin patch and build repo-local zkstack (release).
gl_build_zkstack_cli_release() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-era-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}"
  # SYSCOIN: Cargo is discovered via PATH setup at source time; do not execute
  # HOME-relative shell code while deployment secrets are present.
  local toolchain
  toolchain="${GATEWAY_ZKSTACK_CARGO_TOOLCHAIN:-$(gl_detect_gateway_zkstack_nightly)}"
  [ -n "${toolchain}" ] || gl_die "no nightly Rust toolchain found; install one with rustup"
  local zkstack_cli_dir target_dir zkstack_bin stamp_file
  zkstack_cli_dir="$(cd "${ZKSYNC_ERA_PATH}/zkstack_cli" && pwd -P)" ||
    gl_die "cannot resolve the zkstack_cli workspace"
  target_dir="${zkstack_cli_dir}/target"
  zkstack_bin="${target_dir}/release/zkstack"
  stamp_file="$(gl_zkstack_cli_release_stamp_file)"
  [ "${CARGO_BUILD_TARGET+x}" != x ] ||
    gl_die "CARGO_BUILD_TARGET must be unset for the repo-local zkstack executable"
  # SYSCOIN: Invalidate the exact consumed artifact before Cargo runs. A
  # redirected build must not bless a stale host binary with a fresh stamp.
  rm -f -- "${zkstack_bin}" "${stamp_file}" ||
    gl_die "cannot invalidate the previous zkstack release"
  # SYSCOIN: Override ambient CARGO_TARGET_DIR with the canonical physical
  # repo-local path attested and consumed by the launcher.
  (
    cd "${zkstack_cli_dir}"
    cargo +"${toolchain}" build \
      --release \
      --locked \
      -Znext-lockfile-bump \
      --target-dir "${target_dir}" \
      -p zkstack
  ) || gl_die "Cargo failed to build the repo-local zkstack executable"
  [ -f "${zkstack_bin}" ] && [ -x "${zkstack_bin}" ] && [ ! -L "${zkstack_bin}" ] ||
    gl_die "Cargo did not produce the repo-local zkstack executable"
  gl_write_zkstack_cli_release_stamp
}

gl_zkstack_cli_release_stamp_file() {
  gl_require ZKSYNC_ERA_PATH
  printf '%s\n' "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/.zkstack-syscoin-build-stamp"
}

gl_zkstack_cli_release_fingerprint() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  python3 - "${ZKSYNC_ERA_PATH}" "${ZKSYNC_OS_SERVER_PATH}" "${REQUIRED_ZKSTACK_CLI_SHA:-}" <<'PY'
import hashlib
import json
import subprocess
import sys
from pathlib import Path

era = Path(sys.argv[1])
server = Path(sys.argv[2])
required_sha = sys.argv[3]

def git(args, cwd=era):
    return subprocess.check_output(["git", "-C", str(cwd), *args])

payload = {
    "required_zkstack_cli_sha": required_sha,
    "era_head": git(["rev-parse", "HEAD"]).decode().strip(),
    # Include patched tracked changes because the Syscoin patch is applied on top
    # of the pinned zkstack revision before building the release binary.
    # SYSCOIN: Full index IDs keep this fingerprint clone/object-count independent.
    "zkstack_cli_head_diff_sha256": hashlib.sha256(
        git(["diff", "HEAD", "--full-index", "--binary", "--", "zkstack_cli"])
    ).hexdigest(),
}

for rel in (
    "scripts/apply-zksync-era-syscoin-patch.sh",
    "scripts/patches/zksync-era-syscoin.patch",
    "scripts/patches/era-contracts-syscoin.patch",
):
    path = server / rel
    payload[rel] = hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None

print(hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest())
PY
}

gl_write_zkstack_cli_release_stamp() {
  local stamp_file fingerprint
  stamp_file="$(gl_zkstack_cli_release_stamp_file)"
  fingerprint="$(gl_zkstack_cli_release_fingerprint)"
  mkdir -p "$(dirname "${stamp_file}")"
  printf '%s\n' "${fingerprint}" >"${stamp_file}"
}

gl_ensure_era_contracts_syscoin_postimage() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  # SYSCOIN: zkstack invokes Forge from this mutable contracts checkout. Attest
  # the complete reviewed postimage on every standalone deployment entrypoint,
  # including resumed runs whose L1-deployment checkpoint was already passed.
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-era-contracts-syscoin-patch.sh" \
    "${ZKSYNC_ERA_PATH}/contracts"
}

gl_assert_era_contracts_syscoin_postimage() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-era-contracts-syscoin-patch.sh" \
    --assert-applied "${ZKSYNC_ERA_PATH}/contracts"
}

gl_prepare_gateway_chain_init_contract_artifacts() {
  gl_require GATEWAY_DIR
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  gl_export_foundry_evm_version
  gl_assert_era_contracts_syscoin_postimage || return $?

  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  python3 - \
    "${GATEWAY_DIR}/ZkStack.yaml" \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/ZkStack.yaml" \
    "${ZKSYNC_ERA_PATH}" <<'PY' || return $?
import sys
from pathlib import Path

import yaml

ecosystem_path, chain_path, expected_source = map(Path, sys.argv[1:])
expected_source = expected_source.resolve(strict=True)
expected_contracts = (expected_source / "contracts").resolve(strict=True)

def load(path):
    if path.is_symlink() or not path.is_file():
        raise SystemExit(f"invalid zkstack path config: {path}")
    value = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SystemExit(f"invalid zkstack path config: {path}")
    return value

def exact_path(value, expected, label):
    if not isinstance(value, str) or not Path(value).is_absolute():
        raise SystemExit(f"invalid {label}")
    candidate = Path(value)
    if candidate != expected:
        raise SystemExit(f"{label} does not name the reviewed source tree exactly")
    try:
        resolved = candidate.resolve(strict=True)
    except OSError:
        raise SystemExit(f"invalid {label}") from None
    if resolved != expected:
        raise SystemExit(f"{label} does not match the reviewed source tree")

ecosystem = load(ecosystem_path)
chain = load(chain_path)
exact_path(ecosystem.get("link_to_code"), expected_source, "ecosystem link_to_code")
exact_path(chain.get("link_to_code"), expected_source, "Gateway link_to_code")
exact_path(chain.get("contracts_path"), expected_contracts, "Gateway contracts_path")

# `zkstack dev contracts` is invoked from the ecosystem directory and resolves
# the Era build tree through this optional override before `link_to_code`.
era_source_files = ecosystem.get("era_source_files")
if era_source_files is not None:
    if not isinstance(era_source_files, dict):
        raise SystemExit("invalid ecosystem era_source_files")
    exact_path(
        era_source_files.get("contracts_path"),
        expected_contracts,
        "ecosystem era_source_files.contracts_path",
    )

# Reject redirects before a forced build can clear or write ignored artifacts.
for relative in (
    "l1-contracts/zkout/TransparentUpgradeableProxy.sol/TransparentUpgradeableProxy.json",
    "l1-contracts/zkout/Multicall3.sol/Multicall3.json",
    "l2-contracts/zkout/ForceDeployUpgrader.sol/ForceDeployUpgrader.json",
    "l2-contracts/zkout/ConsensusRegistry.sol/ConsensusRegistry.json",
    "l2-contracts/zkout/TimestampAsserter.sol/TimestampAsserter.json",
):
    current = expected_contracts
    for component in Path(relative).parts:
        current /= component
        if current.is_symlink():
            raise SystemExit(f"symlinked canonical zkout path component: {current}")
        if not current.exists():
            break
PY

  # SYSCOIN: chain init simulates several priority deployments before Forge
  # broadcasts them. Rebuild the ignored zkout inputs from the exact reviewed
  # contracts tree so a stale or missing artifact cannot fail after registration.
  (
    cd "${GATEWAY_DIR}"
    gl_zkstack_pty env \
      FOUNDRY_PROFILE=default \
      FOUNDRY_EVM_VERSION="${FOUNDRY_EVM_VERSION}" \
      FOUNDRY_FORCE=true \
      zkstack dev contracts --l1 --l2
  ) || return $?
  gl_assert_era_contracts_syscoin_postimage || return $?

  python3 - "${ZKSYNC_ERA_PATH}/contracts" <<'PY'
import json
import hashlib
import re
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve(strict=True)
# SHA-256 of creation bytecode forced from the reviewed Syscoin contracts
# postimage above. Even unchanged contracts carry that exact compiler-input
# metadata hash, so clean-upstream cache artifacts are intentionally rejected.
artifacts = {
    "l1-contracts/zkout/TransparentUpgradeableProxy.sol/TransparentUpgradeableProxy.json": "06c66eeddc0a563432cf09c067382656be670c9d1473f07c5a7d8bad1a0278bc",
    "l1-contracts/zkout/Multicall3.sol/Multicall3.json": "336eab43a90ff4027ecb1ca04f44c1224d7ceba4e9fd485735e957205df11192",
    "l2-contracts/zkout/ForceDeployUpgrader.sol/ForceDeployUpgrader.json": "85f5af2cf699393d0f688a6949ed1273484a0172bde9970b87a151a6bb3fc9f5",
    "l2-contracts/zkout/ConsensusRegistry.sol/ConsensusRegistry.json": "5430626e41a7209a421f463a61643e862baff62c21e9af44053cb1aba2211633",
    "l2-contracts/zkout/TimestampAsserter.sol/TimestampAsserter.json": "fd301c8789798b93ce84d5a5a68b3e05455744bc522219f79406d798dda713ae",
}

for relative, expected_sha256 in artifacts.items():
    path = root / relative
    current = root
    for component in Path(relative).parts:
        current /= component
        if current.is_symlink():
            raise SystemExit(f"symlinked canonical zkout path component: {current}")
    if not path.is_file():
        raise SystemExit(f"missing canonical zkout artifact: {path}")
    try:
        artifact = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SystemExit(f"invalid canonical zkout artifact {path}: {error}") from None
    bytecode = artifact.get("bytecode") if isinstance(artifact, dict) else None
    value = bytecode.get("object") if isinstance(bytecode, dict) else None
    if not isinstance(value, str) or re.fullmatch(r"(?:[0-9a-fA-F]{2})+", value) is None:
        raise SystemExit(f"invalid creation bytecode in canonical zkout artifact: {path}")
    raw = bytes.fromhex(value)
    byte_length = len(raw)
    words, remainder = divmod(byte_length, 32)
    if remainder != 0 or words % 2 == 0 or words >= 2**16:
        raise SystemExit(f"invalid zkSync bytecode length in canonical zkout artifact: {path}")
    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != expected_sha256:
        raise SystemExit(
            f"canonical zkout creation bytecode digest mismatch: {path}"
        )
    expected_bytecode_hash = f"0100{words:04x}{actual_sha256[8:]}"
    if artifact.get("hash") != expected_bytecode_hash:
        raise SystemExit(
            f"canonical zkout Era bytecode hash mismatch: {path}"
        )
PY
}

gl_ensure_zkstack_cli_release_current() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  gl_ensure_era_contracts_syscoin_postimage
  # SYSCOIN: Attest the complete pinned zkstack postimage on every entrypoint,
  # including restarts whose release stamp allows the build itself to be skipped.
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-era-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}"
  local zkstack_bin stamp_file expected_fingerprint actual_fingerprint
  zkstack_bin="${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack"
  stamp_file="$(gl_zkstack_cli_release_stamp_file)"
  expected_fingerprint="$(gl_zkstack_cli_release_fingerprint)"
  actual_fingerprint=""
  if [ -f "${stamp_file}" ]; then
    actual_fingerprint="$(tr -d '[:space:]' <"${stamp_file}")"
  fi
  if [ ! -x "${zkstack_bin}" ] || [ "${actual_fingerprint}" != "${expected_fingerprint}" ]; then
    echo "gateway-launch: building zkstack CLI"
    gl_build_zkstack_cli_release
  fi
}

gl_prepare_zksync_era_repo() {
  gl_resolve_required_source_pins
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  local url="${ZKSYNC_ERA_GIT_URL:-https://github.com/matter-labs/zksync-era.git}"

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    mkdir -p "$(dirname "${ZKSYNC_ERA_PATH}")"
    git clone "${url}" "${ZKSYNC_ERA_PATH}"
  fi

  local current_head
  current_head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD)"
  if ! git -C "${ZKSYNC_ERA_PATH}" merge-base --is-ancestor "${REQUIRED_ZKSTACK_CLI_SHA}" "${current_head}" 2>/dev/null; then
    if [ -n "$(git -C "${ZKSYNC_ERA_PATH}" status --porcelain)" ]; then
      gl_die "zksync-era has local changes; cannot check out REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA}"
    fi
    git -C "${ZKSYNC_ERA_PATH}" fetch origin "${REQUIRED_ZKSTACK_CLI_SHA}"
    git -C "${ZKSYNC_ERA_PATH}" checkout "${REQUIRED_ZKSTACK_CLI_SHA}"
  fi

  gl_checkout_contracts_sha
  gl_assert_zksync_era_sha
  gl_assert_contracts_sha
}

gl_prepare_zksync_era_source_repo() {
  gl_require PROTOCOL_VERSION
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  local source_root source_path
  source_root="${ZKSYNC_ERA_SOURCE_ROOT:-${HOME}/.cache/zksync-gateway-era-source}"
  source_path="${source_root}/${PROTOCOL_VERSION}/${REQUIRED_ZKSTACK_CLI_SHA}"
  export ZKSYNC_ERA_SOURCE_PATH="${source_path}"
  echo "gateway-launch: ensuring clean zksync-era source at ${ZKSYNC_ERA_SOURCE_PATH}"

  local saved_path="${ZKSYNC_ERA_PATH:-}"
  export ZKSYNC_ERA_PATH="${ZKSYNC_ERA_SOURCE_PATH}"
  gl_prepare_zksync_era_repo
  local source_top_level_delta
  source_top_level_delta="$(git -C "${ZKSYNC_ERA_PATH}" diff --name-only -- . ":(exclude)contracts")"
  if [ -n "${source_top_level_delta}" ] || [ -n "$(git -C "${ZKSYNC_ERA_PATH}/contracts" status --porcelain)" ]; then
    gl_die "zksync-era source cache is dirty: ${ZKSYNC_ERA_PATH}"
  fi
  export ZKSYNC_ERA_PATH="${saved_path}"
}

gl_workspace_matches_required_pins() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    return 1
  fi

  local top_head contracts_head
  top_head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD 2>/dev/null || true)"
  contracts_head="$(git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD 2>/dev/null || true)"
  [ "${top_head}" = "${REQUIRED_ZKSTACK_CLI_SHA}" ] && [ "${contracts_head}" = "${REQUIRED_CONTRACTS_SHA}" ]
}

gl_workspace_has_resumable_syscoin_release() {
  gl_workspace_matches_required_pins || return 1

  local zkstack_bin stamp_file expected_fingerprint actual_fingerprint
  zkstack_bin="${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack"
  stamp_file="$(gl_zkstack_cli_release_stamp_file)" || return 1
  [ -f "${zkstack_bin}" ] && [ -x "${zkstack_bin}" ] && [ ! -L "${zkstack_bin}" ] || return 1
  [ -f "${stamp_file}" ] && [ ! -L "${stamp_file}" ] || return 1
  gl_assert_era_contracts_syscoin_postimage >/dev/null 2>&1 || return 1
  expected_fingerprint="$(gl_zkstack_cli_release_fingerprint)" || return 1
  actual_fingerprint="$(tr -d '[:space:]' <"${stamp_file}")" || return 1
  [ -n "${expected_fingerprint}" ] && [ "${actual_fingerprint}" = "${expected_fingerprint}" ]
}

# Clone zksync-era if needed, pin top + contracts to versions.yaml, build zkstack if missing.
# If ZKSYNC_ERA_PATH is unset, uses a clean shared source cache and a per-ecosystem mutable working clone.
gl_ensure_zksync_era_workspace() {
  gl_resolve_required_source_pins
  gl_require ZKSYNC_OS_SERVER_PATH
  gl_require PROTOCOL_VERSION
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  if [ -n "${ZKSYNC_ERA_PATH:-}" ]; then
    # SYSCOIN: Preserve only an already sealed Syscoin release. The caller's
    # immediately following postimage applicators perform the full attestation.
    if gl_workspace_has_resumable_syscoin_release; then
      return 0
    fi
    gl_prepare_zksync_era_repo
    return 0
  fi

  gl_prepare_zksync_era_source_repo

  local run_root run_name
  run_root="${ZKSYNC_ERA_RUN_ROOT:-$(dirname "${GATEWAY_DIR:-${HOME}/gateway}")/.zksync-era-workspaces}"
  run_name="${GATEWAY_ECOSYSTEM_NAME:-$(basename "${GATEWAY_DIR:-gateway}")}-${PROTOCOL_VERSION}-${REQUIRED_ZKSTACK_CLI_SHA}"
  export ZKSYNC_ERA_PATH="${run_root}/${run_name}"
  echo "gateway-launch: ZKSYNC_ERA_PATH unset — using mutable workspace ${ZKSYNC_ERA_PATH}"

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    mkdir -p "${run_root}"
    git clone "${ZKSYNC_ERA_SOURCE_PATH}" "${ZKSYNC_ERA_PATH}"
    git -C "${ZKSYNC_ERA_PATH}" submodule update --init --recursive
  fi

  if gl_workspace_matches_required_pins; then
    return 0
  fi

  gl_prepare_zksync_era_repo
}

gl_path_for_zkstack() {
  gl_require ZKSYNC_ERA_PATH
  export PATH="${ZKSYNC_ERA_PATH}/zkstack_cli/target/release:${HOME}/.foundry/bin:${HOME}/.cargo/bin:${PATH}"
}

# zkstack writes the workspace under GATEWAY_ECOSYSTEM_PARENT_DIR using a
# filesystem-safe name (observed: '-' becomes '_'). Resolve an existing output,
# or preselect that output before checkpoint state is keyed by GATEWAY_DIR.
gl_resolve_gateway_dir() {
  gl_require GATEWAY_DIR
  local mode="${1:-existing}" parent eco cand norm

  case "${mode}" in
  existing | planned) ;;
  *) gl_die "invalid Gateway directory resolution mode: ${mode}" ;;
  esac

  parent="${GATEWAY_ECOSYSTEM_PARENT_DIR:-$(dirname "${GATEWAY_DIR}")}"
  parent="$(cd "${parent}" && pwd)"
  # Preserve the caller's logical ecosystem name before planned mode rewrites
  # only its on-disk directory to zkstack's normalized spelling.
  export GATEWAY_ECOSYSTEM_NAME="${GATEWAY_ECOSYSTEM_NAME:-$(basename "${GATEWAY_DIR}")}"
  eco="${GATEWAY_ECOSYSTEM_NAME}"

  if [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
    GATEWAY_DIR="$(cd "${GATEWAY_DIR}" && pwd -P)"
    export GATEWAY_DIR
    return 0
  fi

  cand="${parent}/${eco}"
  if [ -f "${cand}/ZkStack.yaml" ]; then
    export GATEWAY_DIR="$(cd "${cand}" && pwd -P)"
    echo "gateway-launch: ecosystem directory ${GATEWAY_DIR}"
    return 0
  fi

  norm="${eco//-/_}"
  cand="${parent}/${norm}"
  if [ -f "${cand}/ZkStack.yaml" ]; then
    export GATEWAY_DIR="$(cd "${cand}" && pwd -P)"
    echo "gateway-launch: ecosystem directory ${GATEWAY_DIR} (zkstack normalized '${eco}' -> '${norm}')"
    return 0
  fi

  if [ "${mode}" = "planned" ]; then
    export GATEWAY_DIR="${cand}"
    if [ "${eco}" != "${norm}" ]; then
      echo "gateway-launch: planned ecosystem directory ${GATEWAY_DIR} (zkstack normalizes '${eco}' -> '${norm}')"
    fi
    return 0
  fi

  gl_die "after ecosystem create: no ZkStack.yaml under ${parent}/${eco} or ${parent}/${norm} (set GATEWAY_DIR or GATEWAY_ECOSYSTEM_PARENT_DIR explicitly)"
}

# SYSCOIN: Serialize the whole checkpoint/repair lifecycle per planned
# ecosystem directory. Keep the lock in a private sibling directory so a fresh
# launch does not create GATEWAY_DIR before `zkstack ecosystem create` does.
gl_acquire_gateway_launch_lock() {
  gl_require GATEWAY_DIR
  local parent lock_root lock_key lock_file previous_umask
  parent="$(cd "$(dirname "${GATEWAY_DIR}")" && pwd)" || return $?
  lock_root="${parent}/.gateway-launch-locks"
  python3 - "${lock_root}" <<'PY'
import os
import stat
import sys

path = sys.argv[1]
try:
    os.mkdir(path, 0o700)
except FileExistsError:
    pass
info = os.lstat(path)
if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
    raise SystemExit(f"launch lock root must be a non-symlink directory: {path}")
if info.st_uid != os.geteuid():
    raise SystemExit(f"launch lock root must be owned by the launching user: {path}")
os.chmod(path, 0o700)
PY
  lock_key="$(python3 - "${GATEWAY_DIR}" <<'PY'
import hashlib
import os
import sys

print(hashlib.sha256(os.path.realpath(sys.argv[1]).encode("utf-8")).hexdigest())
PY
)" || return $?
  lock_file="${lock_root}/${lock_key}.lock"
  [ ! -L "${lock_file}" ] || gl_die "launch lock must not be a symlink: ${lock_file}"
  if [ -n "${GATEWAY_LAUNCH_LOCK_FD8_KEY:-}" ]; then
    # SYSCOIN: Only our exported path key gives FD 8 lock semantics. PTY and
    # container wrappers may independently leave FD 8 open in the child.
    [ "${GATEWAY_LAUNCH_LOCK_FD8_KEY}" = "${lock_key}" ] ||
      gl_die "inherited Gateway launch lock targets a different deployment"
    python3 - "${lock_file}" 8 <<'PY' || gl_die "inherited FD 8 is not the Gateway launch lock for ${GATEWAY_DIR}"
import fcntl
import os
import stat
import sys

path = sys.argv[1]
fd = int(sys.argv[2])
path_info = os.lstat(path)
fd_info = os.fstat(fd)
if stat.S_ISLNK(path_info.st_mode) or not stat.S_ISREG(path_info.st_mode):
    raise SystemExit(1)
if path_info.st_uid != os.geteuid() or fd_info.st_uid != os.geteuid():
    raise SystemExit(1)
if (path_info.st_dev, path_info.st_ino) != (fd_info.st_dev, fd_info.st_ino):
    raise SystemExit(1)
fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
os.fchmod(fd, 0o600)
PY
    return 0
  fi
  previous_umask="$(umask)"
  umask 077
  exec 8>>"${lock_file}"
  umask "${previous_umask}"
  if ! python3 - "${lock_file}" 8 <<'PY'
import fcntl
import os
import stat
import sys

path = sys.argv[1]
fd = int(sys.argv[2])
path_info = os.lstat(path)
fd_info = os.fstat(fd)
if stat.S_ISLNK(path_info.st_mode) or not stat.S_ISREG(path_info.st_mode):
    raise SystemExit(f"launch lock must be a regular non-symlink file: {path}")
if path_info.st_uid != os.geteuid() or fd_info.st_uid != os.geteuid():
    raise SystemExit(f"launch lock must be owned by the launching user: {path}")
if (path_info.st_dev, path_info.st_ino) != (fd_info.st_dev, fd_info.st_ino):
    raise SystemExit(f"launch lock identity changed while opening: {path}")
os.fchmod(fd, 0o600)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    raise SystemExit(f"another gateway launch/repair owns the lock for {path}") from None
PY
  then
    exec 8>&-
    gl_die "could not acquire launch lock for ${GATEWAY_DIR}"
  fi
  export GATEWAY_LAUNCH_LOCK_FD8_KEY="${lock_key}"
}

# run-gateway-launch uses `exec > >(tee log)`: stdout is a pipe, not a TTY. zkstack/cliclack then
# panics (select.rs NotConnected). util-linux `script` runs the command with a real PTY slave.
gl_zkstack_pty() {
  if [[ "$(uname -s)" == "Linux" ]]; then
    # SYSCOIN: preserve the wrapped zkstack exit status; migration callers rely on set -e.
    script -e -q -c "$(printf '%q ' "$@")" /dev/null
  else
    "$@"
  fi
}

gl_zkstack_private_pty() {
  # SYSCOIN: zkstack may create private-key wallet files before returning.
  # Apply the restrictive umask at creation time, including failure paths.
  (umask 077 && gl_zkstack_pty "$@")
}

gl_assert_chain_contracts_da_preinit_safe() {
  gl_require GATEWAY_DIR
  local chain_name
  chain_name="${1:?chain name required}"
  python3 - \
    "${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml" \
    "${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" <<'PY'
import re
import sys
from pathlib import Path

import yaml

contracts_path = Path(sys.argv[1])
chain_path = Path(sys.argv[2])
if contracts_path.is_symlink():
    raise SystemExit(f"invalid contracts config: {contracts_path}")
if not contracts_path.exists():
    if chain_path.is_symlink() or not chain_path.is_file():
        raise SystemExit(f"invalid chain config: {chain_path}")
    chain = yaml.safe_load(chain_path.read_text(encoding="utf-8"))
    if not isinstance(chain, dict) or chain.get("chain_id") is None:
        raise SystemExit(f"missing chain_id in {chain_path}")
    try:
        chain_id = int(chain["chain_id"])
    except (TypeError, ValueError):
        raise SystemExit(f"invalid chain_id in {chain_path}") from None
    candidate = contracts_path.with_name(f"contracts_{chain_id}.yaml")
    if candidate.is_symlink():
        raise SystemExit(f"invalid contracts config: {candidate}")
    if not candidate.exists():
        # A genuinely fresh chain has no per-chain contracts output yet. The
        # patched zkstack writer emits canonical zero sentinels after register.
        sys.exit(0)
    contracts_path = candidate

if contracts_path.is_symlink() or not contracts_path.is_file():
    raise SystemExit(f"invalid contracts config: {contracts_path}")
contracts_text = contracts_path.read_text(encoding="utf-8")
data = yaml.safe_load(contracts_text)
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {contracts_path}")
raw_data = yaml.load(contracts_text, Loader=yaml.BaseLoader)
if not isinstance(raw_data, dict):
    raise SystemExit(f"invalid raw YAML object in {contracts_path}")

def is_zero_like(value):
    if isinstance(value, bool):
        return False
    if isinstance(value, int):
        return value == 0
    if not isinstance(value, str):
        return False
    raw = value.strip().lower()
    if raw == "0":
        return True
    return re.fullmatch(r"0x0*", raw) is not None

def require_canonical_nonzero_address(value, raw_value, label):
    # PyYAML 1.1 parses an unquoted canonical 0x-prefixed address emitted by
    # zkstack as an integer. Authenticate both the raw canonical spelling and
    # its semantic value so decimalized/corrupted scalars still fail closed.
    if not isinstance(raw_value, str) or re.fullmatch(r"0x[0-9a-f]{40}", raw_value) is None:
        raise SystemExit(f"invalid {label} in {contracts_path} before chain init")
    if isinstance(value, bool):
        raise SystemExit(f"invalid {label} in {contracts_path} before chain init")
    if isinstance(value, int):
        parsed = value
    elif isinstance(value, str) and re.fullmatch(r"0x[0-9a-f]{40}", value) is not None:
        parsed = int(value[2:], 16)
    else:
        raise SystemExit(f"invalid {label} in {contracts_path} before chain init")
    if parsed == 0:
        raise SystemExit(f"zero {label} in {contracts_path} before chain init")
    if parsed < 0 or parsed >= 1 << 160:
        raise SystemExit(f"invalid {label} in {contracts_path} before chain init")
    if parsed != int(raw_value[2:], 16):
        raise SystemExit(f"conflicting {label} in {contracts_path} before chain init")

eco = data.get("ecosystem_contracts")
l1 = data.get("l1")
raw_eco = raw_data.get("ecosystem_contracts")
raw_l1 = raw_data.get("l1")
if not all(isinstance(value, dict) for value in (eco, l1, raw_eco, raw_l1)):
    raise SystemExit(f"missing DA contract sections in {contracts_path} before chain init")
require_canonical_nonzero_address(
    eco.get("rollup_l1_da_validator_addr"),
    raw_eco.get("rollup_l1_da_validator_addr"),
    "ecosystem compact rollup DA validator",
)
require_canonical_nonzero_address(
    l1.get("rollup_l1_da_validator_addr"),
    raw_l1.get("rollup_l1_da_validator_addr"),
    "chain compact rollup DA validator",
)
for field in (
    "blobs_zksync_os_l1_da_validator_addr",
    "no_da_validium_l1_validator_addr",
    "avail_l1_da_validator_addr",
):
    value = l1.get(field)
    if value is not None and not is_zero_like(value):
        raise SystemExit(
            f"unsupported non-zero l1.{field} in {contracts_path}; "
            "refusing chain-init replay before any broadcast"
        )
PY
}

gl_ensure_chain_contracts_yaml_schema() {
  gl_require GATEWAY_DIR
  local chain_name contracts_yaml gateway_chain_name gateway_contracts_yaml
  chain_name="${1:?chain name required}"
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  contracts_yaml="${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml"
  if [ ! -f "${contracts_yaml}" ]; then
    local chain_id contracts_candidate
    chain_id="$(python3 - "${GATEWAY_DIR}/chains/${chain_name}/ZkStack.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
if not path.exists():
    raise SystemExit(f"missing chain config: {path}")
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict) or data.get("chain_id") is None:
    raise SystemExit(f"missing chain_id in {path}")
print(int(data["chain_id"]))
PY
)"
    contracts_candidate="${GATEWAY_DIR}/chains/${chain_name}/configs/contracts_${chain_id}.yaml"
    if [ -f "${contracts_candidate}" ]; then
      # SYSCOIN: zkstack may emit contracts_<chain-id>.yaml before canonical
      # contracts.yaml; only materialize the file matching this chain's ID.
      cp "${contracts_candidate}" "${contracts_yaml}"
      echo "gateway-launch: materialized ${contracts_yaml} from ${contracts_candidate}"
    fi
  fi
  [ -f "${contracts_yaml}" ] || gl_die "missing contracts config: ${contracts_yaml}"
  gateway_contracts_yaml="${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml"

  python3 - "${contracts_yaml}" "${chain_name}" "${gateway_chain_name}" "${gateway_contracts_yaml}" "${GATEWAY_DIR}/configs/initial_deployments.yaml" <<'PY'
import sys
import re
from pathlib import Path

import yaml

contracts_path = Path(sys.argv[1])
chain_name = sys.argv[2]
gateway_chain_name = sys.argv[3]
gateway_contracts_path = Path(sys.argv[4])
initial_deployments_path = Path(sys.argv[5])
contracts_text = contracts_path.read_text(encoding="utf-8")
data = yaml.safe_load(contracts_text)
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {contracts_path}")
raw_data = yaml.load(contracts_text, Loader=yaml.BaseLoader)
if not isinstance(raw_data, dict):
    raw_data = {}

gateway_data = None
if chain_name != gateway_chain_name and gateway_contracts_path.exists():
    gateway_data = yaml.safe_load(gateway_contracts_path.read_text(encoding="utf-8"))
    if not isinstance(gateway_data, dict):
        gateway_data = None

initial_deployments = None
if initial_deployments_path.exists():
    initial_deployments = yaml.safe_load(initial_deployments_path.read_text(encoding="utf-8"))
    if not isinstance(initial_deployments, dict):
        initial_deployments = None

updated = False

def maybe_get(mapping, key):
    if isinstance(mapping, dict):
        value = mapping.get(key)
        if value is not None:
            return value
    return None

l2 = data.get("l2")
if l2 is None:
    l2 = {}
    data["l2"] = l2
    updated = True
if not isinstance(l2, dict):
    raise SystemExit(f"invalid l2 section in {contracts_path}")

if "default_l2_upgrader" not in l2:
    # Backward-compatible default for older generated contracts.yaml files.
    l2["default_l2_upgrader"] = "0x0000000000000000000000000000000000000000"
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        "(added l2.default_l2_upgrader=0x0000000000000000000000000000000000000000)"
    )

if "testnet_paymaster_addr" not in l2:
    # Optional deployment in this flow; keep schema-compliant sentinel when not deployed.
    l2["testnet_paymaster_addr"] = "0x0000000000000000000000000000000000000000"
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        "(added l2.testnet_paymaster_addr=0x0000000000000000000000000000000000000000)"
    )

if "zksys_gas_tank_addr" not in l2:
    # Optional: zero disables zkSYS gas-tank fee payment for the chain.
    l2["zksys_gas_tank_addr"] = "0x0000000000000000000000000000000000000000"
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        "(added l2.zksys_gas_tank_addr=0x0000000000000000000000000000000000000000)"
    )

eco = data.get("ecosystem_contracts")
if eco is None:
    eco = {}
    data["ecosystem_contracts"] = eco
    updated = True
if not isinstance(eco, dict):
    raise SystemExit(f"invalid ecosystem_contracts section in {contracts_path}")

l1 = data.get("l1")
if l1 is None:
    l1 = {}
    data["l1"] = l1
    updated = True
if not isinstance(l1, dict):
    raise SystemExit(f"invalid l1 section in {contracts_path}")

gateway_eco = None
gateway_l1 = None
gateway_l2 = None
if isinstance(gateway_data, dict):
    candidate = gateway_data.get("ecosystem_contracts")
    if isinstance(candidate, dict):
        gateway_eco = candidate
    candidate = gateway_data.get("l1")
    if isinstance(candidate, dict):
        gateway_l1 = candidate
    candidate = gateway_data.get("l2")
    if isinstance(candidate, dict):
        gateway_l2 = candidate

def original_hex_body(value):
    if not isinstance(value, str):
        return None
    s = value.strip()
    if not s.startswith(("0x", "0X")):
        return None
    body = s[2:]
    if body == "" or not re.fullmatch(r"[0-9a-fA-F]+", body):
        return None
    return body

def normalize_scalar(value, raw_value=None):
    # YAML can parse 0x-prefixed scalars as Python ints; convert back to hex
    # to avoid huge decimal string conversion failures when dumping.
    if isinstance(value, int):
        body = format(value, "x")
        raw_body = original_hex_body(raw_value)
        if raw_body is not None:
            body = body.zfill(max(len(body), len(raw_body)))
        return "0x" + body
    return value

def _parse_hex_like(value):
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        # Some corrupted YAMLs contain integer-like floats (e.g. 123.0).
        # Accept only exact integers to avoid silently truncating data.
        if value.is_integer():
            return int(value)
        return None
    if not isinstance(value, str):
        return None
    s = value.strip()
    if s == "":
        return None
    if s.startswith(("0x", "0X")):
        body = s[2:]
        if body == "" or not re.fullmatch(r"[0-9a-fA-F]+", body):
            return None
        return int(body, 16)
    if re.fullmatch(r"[0-9a-fA-F]+", s):
        return int(s, 16)
    # Also accept decimal-encoded integers and float-like integer strings.
    if re.fullmatch(r"[0-9]+", s):
        return int(s, 10)
    if re.fullmatch(r"[0-9]+\.0+", s):
        return int(s.split(".", 1)[0], 10)
    return None

def normalize_address(value):
    parsed = _parse_hex_like(value)
    if parsed is None:
        return normalize_scalar(value)
    if parsed < 0 or parsed >= (1 << 160):
        raise SystemExit(f"invalid address outside 160-bit range: {value}")
    return "0x" + format(parsed, "040x")

def normalize_bytes_hex(value, raw_value=None):
    parsed = _parse_hex_like(value)
    if parsed is None:
        return normalize_scalar(value, raw_value)
    body = format(parsed, "x")
    raw_body = original_hex_body(raw_value)
    if raw_body is not None:
        body = body.zfill(max(len(body), len(raw_body)))
    if len(body) % 2 == 1:
        body = "0" + body
    return "0x" + body

def normalize_h256(value):
    parsed = _parse_hex_like(value)
    if parsed is None:
        return normalize_scalar(value)
    return "0x" + format(parsed & ((1 << 256) - 1), "064x")

def is_zero_like_address(value):
    value = normalize_scalar(value)
    if isinstance(value, int):
        return value == 0
    if not isinstance(value, str):
        return False
    s = value.strip().lower()
    if s in {"0x0", "0x", "0"}:
        return True
    if s.startswith("0x"):
        body = s[2:]
        return body != "" and set(body) == {"0"}
    return False

def pick_value(*candidates, prefer_non_zero=False):
    normalized = [normalize_scalar(v) for v in candidates if v is not None]
    if not normalized:
        return None
    if not prefer_non_zero:
        return normalized[0]
    for v in normalized:
        if not is_zero_like_address(v):
            return v
    return normalized[0]

# Required L2-level schema fields used by current zkstack code paths.
if "da_validator_addr" not in l2:
    l2_da_validator = pick_value(
        maybe_get(gateway_l2, "da_validator_addr"),
        "0x0000000000000000000000000000000000000000",
    )
    l2_da_validator = normalize_address(l2_da_validator)
    l2["da_validator_addr"] = l2_da_validator
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        f"(added l2.da_validator_addr={l2_da_validator})"
    )

# Required top-level fields in current contracts schema.
required_top_level_fields = {
    "create2_factory_addr": True,   # must be a non-zero address
    "create2_factory_salt": False,  # zero is valid
}
for field, prefer_non_zero in required_top_level_fields.items():
    current_value = data.get(field)
    gateway_value = maybe_get(gateway_data, field)
    init_value = maybe_get(initial_deployments, field)
    chosen = pick_value(current_value, gateway_value, init_value, prefer_non_zero=prefer_non_zero)
    if chosen is None:
        raise SystemExit(
            f"unable to auto-heal required top-level field in {contracts_path}: {field}"
        )
    if field.endswith("_addr"):
        chosen = normalize_address(chosen)
    if field.endswith("_salt"):
        chosen = normalize_h256(chosen)
    if current_value != chosen:
        data[field] = chosen
        updated = True
        print(
            f"gateway-launch: patched {contracts_path} for {chain_name} "
            f"(set {field}={chosen})"
        )

# Required core ecosystem fields in current schema.
required_eco_core_fields = (
    "bridgehub_proxy_addr",
    "transparent_proxy_admin_addr",
)
for eco_key in required_eco_core_fields:
    current_value = eco.get(eco_key)
    gateway_value = maybe_get(gateway_eco, eco_key)
    value = pick_value(current_value, gateway_value, prefer_non_zero=True)
    if value is None:
        raise SystemExit(
            f"unable to auto-heal required ecosystem_contracts field in {contracts_path}: {eco_key}"
        )
    value = normalize_address(value)
    if current_value != value:
        eco[eco_key] = value
        updated = True
        print(
            f"gateway-launch: patched {contracts_path} for {chain_name} "
            f"(set ecosystem_contracts.{eco_key}={value})"
        )

# Required CTM fields in ecosystem_contracts for current zkstack parser.
required_eco_fields = {
    "governance": ("governance_addr",),
    "chain_admin": ("chain_admin_addr",),
    "proxy_admin": ("chain_proxy_admin_addr",),
    "state_transition_proxy_addr": (),
    "validator_timelock_addr": ("validator_timelock_addr",),
    "diamond_cut_data": (),
    "l1_bytecodes_supplier_addr": (),
    "server_notifier_proxy_addr": (),
    "default_upgrade_addr": ("default_upgrade_addr",),
    "genesis_upgrade_addr": (),
    "verifier_addr": ("verifier_addr",),
    "l1_rollup_da_manager": (),
}

unresolved = []
address_ctm_fields = {
    "governance",
    "chain_admin",
    "proxy_admin",
    "state_transition_proxy_addr",
    "validator_timelock_addr",
    "l1_bytecodes_supplier_addr",
    "server_notifier_proxy_addr",
    "default_upgrade_addr",
    "genesis_upgrade_addr",
    "verifier_addr",
    "l1_rollup_da_manager",
}
shared_admin_fields = {
    "governance",
    "chain_admin",
    "proxy_admin",
}
for eco_key, l1_keys in required_eco_fields.items():
    current_value = eco.get(eco_key)

    l1_value = None
    for l1_key in l1_keys:
        l1_value = maybe_get(l1, l1_key)
        if l1_value is not None:
            break
    gw_value = maybe_get(gateway_eco, eco_key)
    transparent_proxy_admin_value = maybe_get(eco, "transparent_proxy_admin_addr") if eco_key == "proxy_admin" else None

    # SYSCOIN: shared ecosystem admin principals are not aliases for the target
    # chain's l1.* admin fields. Prefer the gateway ecosystem source and only
    # fall back to l1 values for non-admin CTM fields that are chain-local.
    if eco_key in shared_admin_fields:
        value = pick_value(
            current_value,
            gw_value,
            transparent_proxy_admin_value,
            prefer_non_zero=True,
        )
    else:
        # For address-like CTM fields, avoid keeping zero placeholders from stale edge
        # configs when canonical ecosystem values are available.
        value = pick_value(
            current_value,
            l1_value,
            gw_value,
            transparent_proxy_admin_value,
            prefer_non_zero=(eco_key in address_ctm_fields),
        )

    if value is None:
        unresolved.append(eco_key)
        continue

    if eco_key in address_ctm_fields:
        value = normalize_address(value)
    elif eco_key == "diamond_cut_data":
        value = normalize_bytes_hex(value)

    if current_value != value:
        eco[eco_key] = value
        updated = True
        print(
            f"gateway-launch: patched {contracts_path} for {chain_name} "
            f"(set ecosystem_contracts.{eco_key}={value})"
        )

if unresolved:
    unresolved_csv = ", ".join(unresolved)
    raise SystemExit(
        f"unable to auto-heal required ecosystem_contracts fields in {contracts_path}: {unresolved_csv}"
    )

# SYSCOIN: Preserve the distinct ecosystem-global and chain-local compact
# validators. Unsupported chain-local slots may be absent/zero, but an explicit
# non-zero value is deployment drift; CTM/global alternatives remain intact.
zero_address = "0x0000000000000000000000000000000000000000"

def require_nonzero_address(candidate, label):
    normalized = normalize_address(candidate)
    if not isinstance(normalized, str) or re.fullmatch(r"0x[0-9a-f]{40}", normalized) is None:
        raise SystemExit(f"invalid {label} in {contracts_path}")
    if normalized == zero_address:
        raise SystemExit(f"zero {label} in {contracts_path}")
    return normalized

def choose_rollup(current, fallback, label, require_fallback_match=False):
    current_value = None
    fallback_value = None
    if current is not None and not is_zero_like_address(current):
        current_value = require_nonzero_address(current, label)
    if fallback is not None and not is_zero_like_address(fallback):
        fallback_value = require_nonzero_address(fallback, label)
    if require_fallback_match and current_value and fallback_value and current_value != fallback_value:
        raise SystemExit(f"conflicting {label} in {contracts_path}")
    chosen = current_value or fallback_value
    if chosen is None:
        raise SystemExit(f"missing {label} in {contracts_path}")
    return chosen

if chain_name == gateway_chain_name:
    ecosystem_rollup_fallback = maybe_get(l1, "rollup_l1_da_validator_addr")
    chain_rollup_fallback = maybe_get(eco, "rollup_l1_da_validator_addr")
    require_ecosystem_match = False
else:
    ecosystem_rollup_fallback = maybe_get(gateway_eco, "rollup_l1_da_validator_addr")
    chain_rollup_fallback = None
    require_ecosystem_match = True

ecosystem_rollup = choose_rollup(
    maybe_get(eco, "rollup_l1_da_validator_addr"),
    ecosystem_rollup_fallback,
    "ecosystem compact rollup DA validator",
    require_ecosystem_match,
)
chain_rollup = choose_rollup(
    maybe_get(l1, "rollup_l1_da_validator_addr"),
    chain_rollup_fallback,
    "chain compact rollup DA validator",
)
for section_name, section, value in (
    ("ecosystem_contracts", eco, ecosystem_rollup),
    ("l1", l1, chain_rollup),
):
    if section.get("rollup_l1_da_validator_addr") != value:
        section["rollup_l1_da_validator_addr"] = value
        updated = True
        print(
            f"gateway-launch: patched {contracts_path} for {chain_name} "
            f"(set {section_name}.rollup_l1_da_validator_addr={value})"
        )

for section_name, section, fields in (
    (
        "l1",
        l1,
        (
            "blobs_zksync_os_l1_da_validator_addr",
            "no_da_validium_l1_validator_addr",
            "avail_l1_da_validator_addr",
        ),
    ),
):
    for field in fields:
        current = section.get(field)
        if current is not None and normalize_address(current) != zero_address:
            raise SystemExit(
                f"unsupported non-zero {section_name}.{field} in {contracts_path}"
            )
        if current != zero_address:
            section[field] = zero_address
            updated = True
            print(
                f"gateway-launch: patched {contracts_path} for {chain_name} "
                f"(set {section_name}.{field}={zero_address})"
            )

address_key_hints = {
    "consensus_registry",
    "governance",
    "chain_admin",
    "proxy_admin",
    "l1_rollup_da_manager",
}

def normalize_tree(obj, raw_obj=None, key_hint=None):
    if isinstance(obj, dict):
        raw_mapping = raw_obj if isinstance(raw_obj, dict) else {}
        return {k: normalize_tree(v, raw_mapping.get(k), k) for k, v in obj.items()}
    if isinstance(obj, list):
        raw_items = raw_obj if isinstance(raw_obj, list) else []
        return [
            normalize_tree(v, raw_items[i] if i < len(raw_items) else None, key_hint)
            for i, v in enumerate(obj)
        ]

    if isinstance(key_hint, str):
        if key_hint in address_key_hints or key_hint.endswith("_addr") or key_hint.endswith("_address"):
            return normalize_address(obj)
        if key_hint in {"diamond_cut_data", "force_deployments_data"}:
            return normalize_bytes_hex(obj, raw_obj)
    return normalize_scalar(obj, raw_obj)

normalized_data = normalize_tree(data, raw_data)
if normalized_data != data:
    data = normalized_data
    updated = True

if updated:
    contracts_path.write_text(
        yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )
PY
}

gl_fund_wallets_yaml() {
  gl_require GATEWAY_DIR
  gl_require L1_RPC_URL
  if [ -z "${WALLETS_YAML_PATHS:-}" ]; then
    gl_require WALLETS_YAML_PATH
    WALLETS_YAML_PATHS="${WALLETS_YAML_PATH}"
  fi
  local check_only
  check_only="$(gl_to_lower "${GATEWAY_FUND_CHECK_ONLY:-false}")"
  case "${check_only}" in
  true | false) ;;
  *) gl_die "GATEWAY_FUND_CHECK_ONLY must be true or false" ;;
  esac
  if [ "${check_only}" != true ] && [ -z "${FUNDER_SIGNER:-}" ]; then
    if gl_l1_network_requires_external_signer; then
      FUNDER_SIGNER="account"
    else
      FUNDER_SIGNER="private-key"
    fi
  fi
  if [ "${check_only}" != true ] && [ "$(gl_to_lower "${FUNDER_SIGNER}")" = "private-key" ]; then
    if gl_l1_network_requires_external_signer && ! gl_allow_insecure_private_key_argv; then
      gl_die "FUNDER_SIGNER=private-key is not allowed on ${L1_NETWORK}; import the funder into a Foundry account/keystore, use hardware/KMS signing, or set GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true for an explicit unsafe override"
    fi
    export FUNDER_PRIVATE_KEY="${FUNDER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
  elif [ "${check_only}" != true ] && [ -n "${FUNDER_PRIVATE_KEY:-}" ] && gl_l1_network_requires_external_signer && ! gl_allow_insecure_private_key_argv; then
    gl_die "FUNDER_PRIVATE_KEY is not accepted on ${L1_NETWORK}; use FUNDER_SIGNER with account, keystore, hardware wallet, or KMS signing"
  fi
  if [ "${check_only}" != true ] && [ "$(gl_to_lower "${FUNDER_SIGNER}")" = "keystore" ]; then
    gl_require FUNDER_KEYSTORE
    gl_validate_secret_file "${FUNDER_KEYSTORE}" "FUNDER_KEYSTORE"
  elif [ "${check_only}" != true ] && [ "$(gl_to_lower "${FUNDER_SIGNER}")" = "account" ]; then
    gl_validate_foundry_account_keystore \
      "${FUNDER_ACCOUNT_NAME:-funder}" "FUNDER_ACCOUNT_NAME"
  fi
  if [ "${check_only}" != true ] && [ -n "${FUNDER_PASSWORD_FILE:-}" ]; then
    gl_validate_secret_file "${FUNDER_PASSWORD_FILE}" "FUNDER_PASSWORD_FILE"
  fi
  export FUNDER_SIGNER
  export GATEWAY_FUND_CHECK_ONLY="${check_only}"
  export WALLETS_YAML_PATHS
  python3 - <<'PY'
import os
import shutil
import subprocess
import sys
import time

import yaml
from pathlib import Path

sys.path.insert(0, os.environ["GATEWAY_LAUNCH_HELPER_DIR"])
from _wallet_identity import address_for_private_key, normalize_address

wallet_paths = [Path(p) for p in os.environ["WALLETS_YAML_PATHS"].split(":") if p]
if not wallet_paths:
    raise SystemExit("no wallet files selected for funding")
for path in wallet_paths:
    if not path.is_file():
        raise SystemExit(f"missing wallets file {path}")

rpc = os.environ["L1_RPC_URL"]
l1_network = os.environ.get("L1_NETWORK", "").lower()
funder_signer = os.environ.get("FUNDER_SIGNER", "private-key").lower()
check_only = os.environ.get("GATEWAY_FUND_CHECK_ONLY", "false").lower() == "true"
cast_env = os.environ.copy()
cast_env.pop("FUNDER_PRIVATE_KEY", None)
cast_bin = shutil.which("cast")
if not cast_bin:
    raise SystemExit("cast is required to authenticate and fund wallets")


def env_nonempty(name, default=None):
    value = os.environ.get(name, default)
    if value is None or str(value).strip() == "":
        raise SystemExit(f"{name} must not be empty")
    return str(value)


def funder_wallet_args():
    if funder_signer == "private-key":
        pk = env_nonempty("FUNDER_PRIVATE_KEY")
        return ["--private-key", pk]
    if funder_signer == "account":
        args = ["--account", env_nonempty("FUNDER_ACCOUNT_NAME", "funder")]
    elif funder_signer == "keystore":
        keystore = env_nonempty("FUNDER_KEYSTORE")
        if not Path(keystore).is_file():
            raise SystemExit(f"FUNDER_KEYSTORE does not exist: {keystore}")
        args = ["--keystore", keystore]
    elif funder_signer == "ledger":
        args = ["--ledger"]
    elif funder_signer == "trezor":
        args = ["--trezor"]
    elif funder_signer == "aws":
        args = ["--aws"]
    elif funder_signer == "gcp":
        args = ["--gcp"]
    else:
        raise SystemExit(
            f"unsupported FUNDER_SIGNER={funder_signer}; expected account, keystore, ledger, trezor, aws, gcp, or private-key"
        )

    password_file = os.environ.get("FUNDER_PASSWORD_FILE", "")
    if password_file:
        if not Path(password_file).is_file():
            raise SystemExit(f"FUNDER_PASSWORD_FILE does not exist: {password_file}")
        args.extend(["--password-file", password_file])
    return args


funder_wallet_args = [] if check_only else funder_wallet_args()


def cast_check_output(args):
    return subprocess.check_output(args, text=True, env=cast_env)


def wei_balance(address):
    return int(
        cast_check_output(
            ["cast", "balance", address, "--block", "pending", "--rpc-url", rpc],
        ).strip()
    )


def wei_balance_latest(address):
    return int(
        cast_check_output(
            ["cast", "balance", address, "--rpc-url", rpc],
        ).strip()
    )


def required_balance(role):
    if role == "deployer":
        return int(6 * 10**18)
    if role == "governor":
        if os.environ.get("GATEWAY_FUND_GOVERNOR_BALANCE_WEI"):
            return int(os.environ["GATEWAY_FUND_GOVERNOR_BALANCE_WEI"])
        return int(11 * 10**18)
    return int(10**18)


default_send_timeout = "900" if l1_network in {"tanenbaum", "mainnet"} else "45"
default_rpc_timeout = "120" if l1_network in {"tanenbaum", "mainnet"} else "45"
default_min_topup_wei = str(25 * 10**16) if l1_network in {"tanenbaum", "mainnet"} else "0"
default_post_fund_wait_timeout = "2400" if l1_network in {"tanenbaum", "mainnet"} else "120"
default_post_fund_poll_interval = "5"
send_timeout = os.environ.get("GATEWAY_FUND_TX_TIMEOUT", default_send_timeout)
rpc_timeout = os.environ.get("GATEWAY_FUND_RPC_TIMEOUT", default_rpc_timeout)
min_topup_wei = int(os.environ.get("GATEWAY_FUND_MIN_TOPUP_WEI", default_min_topup_wei))
post_fund_wait_timeout = int(
    os.environ.get("GATEWAY_FUND_POST_WAIT_TIMEOUT", default_post_fund_wait_timeout)
)
post_fund_poll_interval = float(
    os.environ.get("GATEWAY_FUND_POST_WAIT_POLL_INTERVAL", default_post_fund_poll_interval)
)


recipients = {}
server_signer_roles = {"operator", "blob_operator", "prove_operator", "execute_operator"}
for wallet_path in wallet_paths:
    w = yaml.safe_load(wallet_path.read_text())
    if not isinstance(w, dict) or not w:
        raise SystemExit(f"invalid wallets yaml object in {wallet_path}")
    for role, cfg in w.items():
        if role == "test_wallet":
            continue
        if not isinstance(cfg, dict) or "address" not in cfg:
            raise SystemExit(f"invalid wallet entry {role} in {wallet_path}")
        address = normalize_address(
            cfg["address"], f"{role}.address in {wallet_path}"
        )
        if "private_key" in cfg and cfg["private_key"] is not None:
            derived = address_for_private_key(
                cfg["private_key"], f"{role}.private_key in {wallet_path}", cast_bin
            )
            if derived != address:
                raise SystemExit(
                    f"{role} address/private-key mismatch in {wallet_path}: "
                    f"configured={address} derived={derived}"
                )
        elif role in server_signer_roles:
            raise SystemExit(
                f"missing private key for required server signer {role} in {wallet_path}"
            )
        key = address.lower()
        target = required_balance(role)
        existing = recipients.get(key)
        label = f"{wallet_path}:{role}"
        if existing is None or target > existing["target"]:
            labels = [] if existing is None else existing["labels"]
            recipients[key] = {
                "role": role,
                "address": address,
                "target": target,
                "labels": [*labels, label],
            }
        else:
            existing["labels"].append(label)

if not recipients:
    raise SystemExit("no fundable wallet entries found in selected wallet files")

if check_only:
    below_target = []
    for recipient in recipients.values():
        current = wei_balance_latest(recipient["address"])
        deficit = max(0, recipient["target"] - current)
        if deficit >= min_topup_wei and deficit != 0:
            below_target.append(
                f"{','.join(recipient['labels'])}: current={current} "
                f"target={recipient['target']} deficit={deficit}"
            )
    if below_target:
        raise SystemExit("wallets below required latest balance: " + "; ".join(below_target))
    print("all wallets meet required balances within the configured dust tolerance")
    raise SystemExit(0)

funder = cast_check_output(["cast", "wallet", "address", *funder_wallet_args]).strip()
funder_balance = wei_balance(funder)
starting_nonce = int(
    cast_check_output(
        ["cast", "nonce", funder, "--block", "pending", "--rpc-url", rpc],
    ).strip()
)

transfers = []
wait_only = []
for recipient in recipients.values():
    role = recipient["role"]
    address = recipient["address"]
    target = required_balance(role)
    target = recipient["target"]
    current_latest = wei_balance_latest(address)
    current_pending = wei_balance(address)
    deficit = max(0, target - current_latest)
    label = ",".join(recipient["labels"])
    if deficit == 0:
        print(
            f"wallet {label} already funded on latest: current_latest={current_latest} "
            f"current_pending={current_pending} target={target}"
        )
        continue
    if current_pending >= target:
        print(
            f"wallet {label} has pending top-up in-flight: current_latest={current_latest} "
            f"current_pending={current_pending} target={target}; waiting for confirmation"
        )
        wait_only.append((label, address, current_latest, target))
        continue
    if deficit < min_topup_wei:
        print(
            f"wallet {label} below target by dust: current_latest={current_latest} "
            f"current_pending={current_pending} target={target} "
            f"deficit={deficit} min_topup_wei={min_topup_wei}; skipping top-up"
        )
        continue
    transfers.append((label, address, current_latest, target, deficit))

if not transfers and not wait_only:
    print("all wallets already meet required balances on latest; skipping funding")
    raise SystemExit(0)

total_deficit = sum(deficit for _, _, _, _, deficit in transfers)
if funder_balance < total_deficit:
    raise SystemExit(
        f"funder {funder} has insufficient balance: balance={funder_balance} total_required={total_deficit}"
    )

for index, (role, address, current, target, deficit) in enumerate(transfers):
    nonce = starting_nonce + index
    result = cast_check_output(
        [
            "cast",
            "send",
            address,
            "--value",
            str(deficit),
            "--rpc-url",
            rpc,
            "--rpc-timeout",
            rpc_timeout,
            "--timeout",
            send_timeout,
            *funder_wallet_args,
            "--nonce",
            str(nonce),
            "--async",
        ]
    ).strip()
    print(
        f"funding wallet {role}: current={current} target={target} deficit={deficit} "
        f"nonce={nonce} tx={result}"
    )

deadline = time.time() + post_fund_wait_timeout
pending_targets = {role: (address, target) for role, address, _, target in wait_only}
for role, address, _, target, _ in transfers:
    pending_targets[role] = (address, target)
print(
    f"waiting for funding transactions to be reflected on latest block for {len(pending_targets)} wallet(s) "
    f"(timeout={post_fund_wait_timeout}s)"
)

while pending_targets:
    completed_roles = []
    for role, (address, target) in pending_targets.items():
        current_latest = wei_balance_latest(address)
        if current_latest >= target:
            print(f"wallet {role} funded on latest: current={current_latest} target={target}")
            completed_roles.append(role)
    for role in completed_roles:
        pending_targets.pop(role, None)
    if not pending_targets:
        break
    if time.time() > deadline:
        missing = ", ".join(sorted(pending_targets.keys()))
        raise SystemExit(
            f"timed out waiting for wallet funding confirmations on latest block; still below target: {missing}"
        )
    time.sleep(post_fund_poll_interval)
PY
}

gl_resolve_syscoin_cookie_file() {
  local cookie_file datadir network candidate
  cookie_file="${BITCOIN_DA_COOKIE_FILE:-}"
  if [ -n "${cookie_file}" ] && [ -f "${cookie_file}" ]; then
    printf '%s\n' "${cookie_file}"
    return 0
  fi

  datadir="${SYSCOIN_DATADIR:-${HOME}/.syscoin}"
  network="${SYSCOIN_NETWORK:-}"
  if [ -n "${network}" ]; then
    candidate="${datadir}/${network}/.cookie"
    if [ -f "${candidate}" ]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  fi

  candidate="${datadir}/testnet3/.cookie"
  if [ -f "${candidate}" ]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  return 1
}

gl_load_bitcoin_da_cookie_credentials() {
  local cookie_file cookie
  if [ -n "${BITCOIN_DA_RPC_USER:-}" ] && [ -n "${BITCOIN_DA_RPC_PASSWORD:-}" ]; then
    return 0
  fi

  cookie_file="$(gl_resolve_syscoin_cookie_file || true)"
  [ -n "${cookie_file}" ] || return 0
  cookie="$(< "${cookie_file}")"
  : "${BITCOIN_DA_RPC_USER:=${cookie%%:*}}"
  : "${BITCOIN_DA_RPC_PASSWORD:=${cookie#*:}}"
  export BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD
}

gl_refresh_bitcoin_da_config_from_cookie() {
  local config_path="${1:?config path required}"
  local cookie_file
  [ -f "${config_path}" ] || gl_die "missing os-server config: ${config_path}"

  cookie_file="$(gl_resolve_syscoin_cookie_file || true)"
  if [ -z "${cookie_file}" ]; then
    if grep -Eq '^[[:space:]]*bitcoin_da_rpc_(user|password):' "${config_path}"; then
      echo "gateway-launch: Syscoin cookie not found; using existing Bitcoin DA RPC credentials in ${config_path}" >&2
    fi
    return 0
  fi

  python3 - "${config_path}" "${cookie_file}" <<'PY'
import json
import re
import sys
from pathlib import Path

config_path = Path(sys.argv[1])
cookie_path = Path(sys.argv[2])
cookie = cookie_path.read_text(encoding="utf-8").rstrip("\r\n")
rpc_user, separator, rpc_password = cookie.partition(":")
if separator != ":" or not rpc_user or not rpc_password:
    raise SystemExit(f"invalid Syscoin RPC cookie format in {cookie_path}")

text = config_path.read_text(encoding="utf-8")
text, user_count = re.subn(
    r"^(\s*bitcoin_da_rpc_user:\s*).*$",
    lambda m: f"{m.group(1)}{json.dumps(rpc_user)}",
    text,
    count=1,
    flags=re.MULTILINE,
)
text, password_count = re.subn(
    r"^(\s*bitcoin_da_rpc_password:\s*).*$",
    lambda m: f"{m.group(1)}{json.dumps(rpc_password)}",
    text,
    count=1,
    flags=re.MULTILINE,
)
if user_count == 0 and password_count == 0:
    raise SystemExit(0)
if user_count != 1 or password_count != 1:
    raise SystemExit(f"failed to patch Syscoin RPC credentials in {config_path}")

config_path.write_text(text, encoding="utf-8")
print(f"gateway-launch: refreshed Syscoin RPC credentials in {config_path}")
PY
}

gl_prepare_bitcoin_da_wallet() {
  : "${BITCOIN_DA_WALLET_NAME:=zksync-os}"
  : "${BITCOIN_DA_ADDRESS_LABEL:=zksync-os-batcher}"
  : "${BITCOIN_DA_MIN_BALANCE_SYS:=0}"
  gl_require BITCOIN_DA_RPC_URL
  gl_load_bitcoin_da_cookie_credentials
  gl_require BITCOIN_DA_RPC_USER
  gl_require BITCOIN_DA_RPC_PASSWORD
  export BITCOIN_DA_RPC_URL BITCOIN_DA_RPC_USER BITCOIN_DA_RPC_PASSWORD
  export BITCOIN_DA_WALLET_NAME BITCOIN_DA_ADDRESS_LABEL BITCOIN_DA_MIN_BALANCE_SYS

  python3 - <<'PY'
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from decimal import Decimal, InvalidOperation


class RpcError(Exception):
    def __init__(self, code, message):
        super().__init__(f"RPC error {code}: {message}")
        self.code = code
        self.message = str(message)


rpc_url = os.environ["BITCOIN_DA_RPC_URL"].rstrip("/")
rpc_user = os.environ["BITCOIN_DA_RPC_USER"]
rpc_password = os.environ["BITCOIN_DA_RPC_PASSWORD"]
wallet_name = os.environ["BITCOIN_DA_WALLET_NAME"]
address_label = os.environ["BITCOIN_DA_ADDRESS_LABEL"]

try:
    min_balance = Decimal(os.environ["BITCOIN_DA_MIN_BALANCE_SYS"])
except (InvalidOperation, KeyError) as exc:
    raise SystemExit(f"invalid BITCOIN_DA_MIN_BALANCE_SYS: {exc}")
if min_balance < 0:
    raise SystemExit("BITCOIN_DA_MIN_BALANCE_SYS must be >= 0")


def rpc(method, params=None, wallet=None):
    url = rpc_url
    if wallet is not None:
        url = f"{url}/wallet/{urllib.parse.quote(wallet, safe='')}"
    payload = json.dumps(
        {"jsonrpc": "2.0", "method": method, "params": params or [], "id": 1}
    ).encode("utf-8")
    auth = base64.b64encode(f"{rpc_user}:{rpc_password}".encode("utf-8")).decode("ascii")
    req = urllib.request.Request(
        url,
        data=payload,
        headers={
            "Authorization": f"Basic {auth}",
            "Content-Type": "application/json",
            "User-Agent": "gateway-launch/1.0",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = resp.read().decode("utf-8")
    except urllib.error.HTTPError as err:
        body = err.read().decode("utf-8", errors="replace")
        try:
            obj = json.loads(body)
            if isinstance(obj, dict) and obj.get("error"):
                rpc_err = obj["error"]
                raise RpcError(rpc_err.get("code"), rpc_err.get("message")) from err
        except json.JSONDecodeError:
            pass
        raise SystemExit(f"Syscoin RPC {method} HTTP {err.code}: {body}") from err

    obj = json.loads(body)
    if obj.get("error"):
        rpc_err = obj["error"]
        raise RpcError(rpc_err.get("code"), rpc_err.get("message"))
    return obj.get("result")


def load_or_create_wallet(name, *, create):
    try:
        rpc("loadwallet", [name])
        print(f"gateway-launch: loaded Syscoin wallet {name}")
        return
    except RpcError as err:
        msg = err.message.lower()
        if err.code == -4 or "already loaded" in msg:
            print(f"gateway-launch: Syscoin wallet {name} already loaded")
            return
        if err.code != -18 and "not found" not in msg and "does not exist" not in msg:
            raise
        if not create:
            raise SystemExit(f"required Syscoin wallet {name} does not exist")

    try:
        rpc("createwallet", [name])
        print(f"gateway-launch: created Syscoin wallet {name}")
    except RpcError as err:
        if err.code == -4 or "already loaded" in err.message.lower():
            print(f"gateway-launch: Syscoin wallet {name} already loaded")
            return
        raise


def labelled_address(name, label):
    try:
        addresses = rpc("getaddressesbylabel", [label], wallet=name)
    except RpcError as err:
        if err.code != -11:
            raise
        address = rpc("getnewaddress", [label], wallet=name)
        print(f"gateway-launch: created Syscoin DA address {address} label={label}")
        return address
    if not isinstance(addresses, dict) or not addresses:
        address = rpc("getnewaddress", [label], wallet=name)
        print(f"gateway-launch: created Syscoin DA address {address} label={label}")
        return address
    address = next(iter(addresses.keys()))
    print(f"gateway-launch: reusing Syscoin DA address {address} label={label}")
    return address


def wallet_balance(name):
    return Decimal(str(rpc("getbalance", [], wallet=name)))


load_or_create_wallet(wallet_name, create=True)
address = labelled_address(wallet_name, address_label)
balance = wallet_balance(wallet_name)
print(f"gateway-launch: Syscoin DA wallet {wallet_name} balance={balance} SYS target={min_balance} SYS")

if balance >= min_balance:
    raise SystemExit(0)
if min_balance == 0:
    print(
        "gateway-launch: BITCOIN_DA_MIN_BALANCE_SYS=0; wallet/address ensured but funding is disabled"
    )
    raise SystemExit(0)
raise SystemExit(
    f"Syscoin DA wallet {wallet_name} is below target ({balance} < {min_balance}). "
    f"Fund this DA wallet address directly: {address}"
)
PY
}

# -----------------------------
# Checkpoint state management
# -----------------------------

gl_checkpoint_state_dir() {
  gl_require GATEWAY_DIR
  local legacy_state parent state_key
  legacy_state="${GATEWAY_DIR}/.gateway-launch/state.json"
  if [ -e "${legacy_state}" ] || [ -L "${legacy_state}" ]; then
    [ -f "${legacy_state}" ] && [ ! -L "${legacy_state}" ] ||
      gl_die "unsafe legacy checkpoint state file: ${legacy_state}"
    printf '%s\n' "${GATEWAY_DIR}/.gateway-launch"
    return 0
  fi
  parent="$(cd "$(dirname "${GATEWAY_DIR}")" && pwd)" || return $?
  state_key="$(python3 - "${GATEWAY_DIR}" <<'PY'
import hashlib
import os
import sys

print(hashlib.sha256(os.path.realpath(sys.argv[1]).encode("utf-8")).hexdigest())
PY
)" || return $?
  printf '%s/.gateway-launch-state/%s\n' "${parent}" "${state_key}"
}

gl_checkpoint_state_file() {
  printf '%s/state.json\n' "$(gl_checkpoint_state_dir)"
}

gl_checkpoint_state_init() {
  local state_dir state_file
  state_dir="$(gl_checkpoint_state_dir)" || return $?
  state_file="$(gl_checkpoint_state_file)" || return $?
  python3 - "${state_dir}" "${state_file}" <<'PY'
import json
import os
import stat
import sys
import uuid
from datetime import datetime, timezone

state_dir, state_path = sys.argv[1:]
try:
    os.makedirs(state_dir, mode=0o700, exist_ok=True)
except OSError as exc:
    raise SystemExit(f"failed to create checkpoint state directory {state_dir}: {exc}")
dir_info = os.lstat(state_dir)
if stat.S_ISLNK(dir_info.st_mode) or not stat.S_ISDIR(dir_info.st_mode):
    raise SystemExit(f"checkpoint state directory is unsafe: {state_dir}")
if dir_info.st_uid != os.geteuid():
    raise SystemExit(f"checkpoint state directory has wrong owner: {state_dir}")
os.chmod(state_dir, 0o700)

try:
    info = os.lstat(state_path)
except FileNotFoundError:
    info = None
if info is not None:
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise SystemExit(f"checkpoint state file is unsafe: {state_path}")
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise SystemExit(f"checkpoint state file ownership/permissions are unsafe: {state_path}")
    raise SystemExit(0)

now = datetime.now(timezone.utc).isoformat()
state = {
    "schema_version": 1,
    "run_id": str(uuid.uuid4()),
    "created_at": now,
    "updated_at": now,
    "current_checkpoint": None,
    "fingerprint": {},
    "checkpoints": {},
    "last_error": None,
    "repairs": [],
}
flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
if hasattr(os, "O_NOFOLLOW"):
    flags |= os.O_NOFOLLOW
fd = os.open(state_path, flags, 0o600)
with os.fdopen(fd, "w", encoding="utf-8") as handle:
    json.dump(state, handle, indent=2, sort_keys=True)
    handle.write("\n")
    handle.flush()
    os.fsync(handle.fileno())
PY
}

gl_checkpoint_fingerprint_json() {
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA
  gl_require L1_CHAIN_ID
  gl_require L1_NETWORK
  gl_require GATEWAY_DIR
  gl_normalize_canonical_deployment_inputs
  local gateway_settlement_fee published_gateway_commit_target published_gateway_relay
  gateway_settlement_fee="$(gl_effective_gateway_settlement_fee)" || return $?
  published_gateway_commit_target="$(gl_published_gateway_commit_target)" || return $?
  published_gateway_relay="$(gl_published_gateway_relay)" || return $?
  GL_EFFECTIVE_GATEWAY_SETTLEMENT_FEE="${gateway_settlement_fee}" \
  GL_PUBLISHED_GATEWAY_COMMIT_TARGET="${published_gateway_commit_target}" \
  GL_PUBLISHED_GATEWAY_RELAY="${published_gateway_relay}" \
  python3 - <<'PY'
import hashlib
import json
import os

def h(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()

def raw_effective(name: str, default: str = "") -> str:
    value = os.environ.get(name)
    return default if value is None or value == "" else value

def trimmed_effective(name: str, default: str = "") -> str:
    return raw_effective(name, default).strip()

def normalize_bool(name: str, default: str) -> str:
    value = raw_effective(name, default).lower()
    if value not in {"true", "false"}:
        raise SystemExit(f"{name} must be true or false")
    return value

def normalize_address(name: str, default: str = "") -> str:
    raw = trimmed_effective(name, default)
    if not raw:
        return ""
    if not raw.startswith(("0x", "0X")) or len(raw) != 42:
        raise SystemExit(f"{name} must be a 20-byte hex address")
    return "0x" + format(int(raw[2:], 16), "040x")

def normalize_nonzero_address(name: str, default: str = "") -> str:
    value = normalize_address(name, default)
    if not value or value == "0x" + "0" * 40:
        raise SystemExit(f"{name} must not be zero")
    return value

def normalize_bytes32(name: str, default: str) -> str:
    raw = trimmed_effective(name, default)
    if raw.startswith(("0x", "0X")):
        value = int(raw[2:] or "0", 16)
    elif raw.isdecimal():
        value = int(raw, 10)
    else:
        value = int(raw, 16)
    if value < 0 or value >= 1 << 256:
        raise SystemExit(f"{name} must fit bytes32")
    return "0x" + format(value, "064x")

def normalize_uint(name: str, default: str, maximum: int) -> str:
    raw = trimmed_effective(name, default)
    if not raw.isdecimal():
        raise SystemExit(f"{name} must be an unsigned decimal integer")
    value = int(raw, 10)
    if value > maximum:
        raise SystemExit(f"{name} must be <= {maximum}")
    return str(value)

def normalize_nonzero_uint(name: str, default: str, maximum: int) -> str:
    value = normalize_uint(name, default, maximum)
    if value == "0":
        raise SystemExit(f"{name} must be positive")
    return value

def normalize_gateway_create2_salt() -> str:
    raw = trimmed_effective("GATEWAY_CREATE2_FACTORY_SALT")
    if not raw:
        return ""
    if raw.startswith(("0x", "0X")):
        hex_value = raw[2:]
        if len(hex_value) == 0 or len(hex_value) > 64:
            raise SystemExit(
                "GATEWAY_CREATE2_FACTORY_SALT hex length must be 1..64 nybbles"
            )
        value = int(hex_value, 16)
    else:
        value = int(raw, 10)
    if value < 0 or value >= 1 << 256:
        raise SystemExit("GATEWAY_CREATE2_FACTORY_SALT must fit uint256")
    return "0x" + format(value, "064x")

gateway_dir = raw_effective("GATEWAY_DIR")
l1_network = raw_effective("L1_NETWORK").lower()
l1_chain_id = normalize_uint("L1_CHAIN_ID", "0", (1 << 256) - 1)
bridge_enabled_raw = normalize_bool("ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE", "true")
bridge_enabled = bridge_enabled_raw == "true"

weth_defaults = {
    "5700": "0xa66b2e50c2b805f31712bea422d0d9e7d0fd0f35",
    "57": "0xd3e822f3ef011ca5f17d82c956d952d8d7c3a1bb",
}
l1_weth_token_address = normalize_address(
    "L1_WETH_TOKEN_ADDRESS", weth_defaults.get(l1_chain_id, "")
)
if l1_network in {"tanenbaum", "mainnet"} and (
    not l1_weth_token_address or l1_weth_token_address == "0x" + "0" * 40
):
    raise SystemExit("L1_WETH_TOKEN_ADDRESS must not be zero")

zksys_l2_deployment = {}
if bridge_enabled or l1_network in {"tanenbaum", "mainnet"}:
    token_admin = normalize_nonzero_address("ZKSYS_L2_TOKEN_ADMIN_ADDRESS")
    zksys_l2_deployment.update(
        {
            "create2_deployer": normalize_nonzero_address(
                "ZKSYS_L2_CREATE2_DEPLOYER",
                "0x4e59b44847b379578588920ca78fbf26c0b4956c",
            ),
            "token_admin": token_admin,
            "proxy_admin_salt": normalize_bytes32(
                "ZKSYS_L2_PROXY_ADMIN_SALT",
                "0x7a6b7379732d70726f78792d61646d696e000000000000000000000000000000",
            ),
        }
    )
if bridge_enabled:
    zksys_l2_deployment.update(
        {
            "registry_impl_salt": normalize_bytes32(
                "ZKSYS_L2_REGISTRY_IMPL_SALT",
                "0x7a6b7379732d72656769737472792d696d706c00000000000000000000000000",
            ),
            "registry_proxy_salt": normalize_bytes32(
                "ZKSYS_L2_REGISTRY_PROXY_SALT",
                "0x7a6b7379732d72656769737472792d70726f7879000000000000000000000000",
            ),
        }
    )
if l1_network in {"tanenbaum", "mainnet"}:
    zksys_l2_deployment.update(
        {
            "token_impl_salt": normalize_bytes32(
                "ZKSYS_L2_TOKEN_IMPL_SALT",
                "0x7a6b7379732d746f6b656e2d696d706c00000000000000000000000000000000",
            ),
            "token_proxy_salt": normalize_bytes32(
                "ZKSYS_L2_TOKEN_PROXY_SALT",
                "0x7a6b7379732d746f6b656e2d70726f7879000000000000000000000000000000",
            ),
            "token_name": raw_effective("ZKSYS_L2_TOKEN_NAME", "ZKSYS"),
            "token_symbol": raw_effective("ZKSYS_L2_TOKEN_SYMBOL", "ZKSYS"),
            "token_decimals": normalize_uint(
                "ZKSYS_L2_TOKEN_DECIMALS", "18", 59
            ),
        }
    )

zksys_l1_registry_bridge = {"enabled": bridge_enabled}
if bridge_enabled:
    zksys_l1_registry_bridge.update(
        {
            "proxy_admin_owner": normalize_nonzero_address(
                "ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS",
                token_admin,
            ),
            "bridge_proxy_admin_salt": normalize_bytes32(
                "ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_SALT",
                "0x7a6b7379732d6c312d72656769737472792d6272696467652d61646d696e0000",
            ),
            "bridge_impl_salt": normalize_bytes32(
                "ZKSYS_L1_REGISTRY_BRIDGE_IMPL_SALT",
                "0x7a6b7379732d6c312d72656769737472792d6272696467652d696d706c000000",
            ),
            "bridge_proxy_salt": normalize_bytes32(
                "ZKSYS_L1_REGISTRY_BRIDGE_PROXY_SALT",
                "0x7a6b7379732d6c312d72656769737472792d6272696467652d70726f78790000",
            ),
            "nevm_start_block": normalize_nonzero_uint(
                "ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK",
                "1317500",
                (1 << 32) - 1,
            ),
            "seniority_height1": normalize_uint(
                "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1",
                "210240",
                (1 << 32) - 1,
            ),
            "seniority_height2": normalize_uint(
                "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT2",
                "525600",
                (1 << 32) - 1,
            ),
            "seniority_level1_bps": normalize_uint(
                "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS",
                "0",
                10_000,
            ),
            "seniority_level2_bps": normalize_uint(
                "ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS",
                "0",
                10_000,
            ),
        }
    )

    height1 = int(zksys_l1_registry_bridge["seniority_height1"])
    height2 = int(zksys_l1_registry_bridge["seniority_height2"])
    level1 = int(zksys_l1_registry_bridge["seniority_level1_bps"])
    level2 = int(zksys_l1_registry_bridge["seniority_level2_bps"])
    if height1 == 0 or height2 <= height1 or level2 < level1:
        raise SystemExit("invalid zkSYS L1 registry bridge seniority config")

if normalize_bool("USE_DUMMY_MESSAGE_ROOT", "false") != "false":
    raise SystemExit(
        "USE_DUMMY_MESSAGE_ROOT=true is forbidden for canonical Syscoin deployments"
    )
if l1_network in {"tanenbaum", "mainnet"} and (
    trimmed_effective("ZKSYS_ZK_TOKEN_ASSET_ID")
    or trimmed_effective("ZK_TOKEN_ASSET_ID")
):
    raise SystemExit(
        "ZKSYS_ZK_TOKEN_ASSET_ID/ZK_TOKEN_ASSET_ID are derived for canonical Syscoin deployments"
    )

canonical_create2 = "0x4e59b44847b379578588920ca78fbf26c0b4956c"
if zksys_l2_deployment and zksys_l2_deployment["create2_deployer"] != canonical_create2:
    raise SystemExit("ZKSYS_L2_CREATE2_DEPLOYER must be the canonical Arachnid factory")

payload = {
    "protocol_version": raw_effective("PROTOCOL_VERSION", "v32.0"),
    "required_zkstack_cli_sha": os.environ.get("REQUIRED_ZKSTACK_CLI_SHA", ""),
    "required_contracts_sha": os.environ.get("REQUIRED_CONTRACTS_SHA", ""),
    "l1_chain_id": l1_chain_id,
    "l1_network": l1_network,
    "l1_rpc_url_hash": h(os.environ.get("L1_RPC_URL", "")),
    "l1_weth_token_address": l1_weth_token_address,
    "gateway_dir": gateway_dir,
    "gateway_chain_name": raw_effective("GATEWAY_CHAIN_NAME", "gateway"),
    "gateway_chain_id": normalize_nonzero_uint(
        "GATEWAY_CHAIN_ID", "57001", (1 << 32) - 1
    ),
    "gateway_commit_mode": raw_effective("GATEWAY_COMMIT_MODE", "rollup"),
    "gateway_settlement_fee": os.environ["GL_EFFECTIVE_GATEWAY_SETTLEMENT_FEE"],
    # SYSCOIN: Resume state is bound to both consensus-authenticated Gateway
    # endpoints so a candidate repin cannot inherit an older launch checkpoint.
    "published_gateway_commit_target": normalize_nonzero_address(
        "GL_PUBLISHED_GATEWAY_COMMIT_TARGET"
    ),
    "published_gateway_relay": normalize_nonzero_address(
        "GL_PUBLISHED_GATEWAY_RELAY"
    ),
    "edge_chain_name": raw_effective("EDGE_CHAIN_NAME", "zksys"),
    "edge_chain_id": normalize_nonzero_uint(
        "EDGE_CHAIN_ID", "57057", (1 << 32) - 1
    ),
    "prover_mode": os.environ.get("PROVER_MODE", ""),
    "gateway_prover_mode": os.environ.get("GATEWAY_PROVER_MODE", ""),
    "zksync_os_mock_verifier": normalize_bool(
        "SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER", "false"
    ),
    "edge_prover_mode": raw_effective(
        "EDGE_PROVER_MODE", os.environ.get("PROVER_MODE", "")
    ),
    "edge_reuse_gateway_governor": normalize_bool(
        "EDGE_REUSE_GATEWAY_GOVERNOR", "true"
    ),
    "gateway_l2_da_commitment_scheme_value": normalize_uint(
        "GATEWAY_L2_DA_COMMITMENT_SCHEME_VALUE", "4", 255
    ),
    "edge_gateway_committer_wallet_name": raw_effective(
        "EDGE_GATEWAY_COMMITTER_WALLET_NAME", "blob_operator"
    ),
    "foundry_evm_version": os.environ.get("FOUNDRY_EVM_VERSION", ""),
    "gateway_create2_factory_salt": normalize_gateway_create2_salt(),
    "gateway_create2_factory_address": canonical_create2,
    "zksys_l1_registry_bridge": zksys_l1_registry_bridge,
}
if zksys_l2_deployment:
    payload["zksys_l2_deployment"] = zksys_l2_deployment
print(json.dumps(payload, sort_keys=True))
PY
}

gl_checkpoint_set_fingerprint_if_empty() {
  gl_checkpoint_state_init || return $?
  local state_file fp_json
  state_file="$(gl_checkpoint_state_file)" || return $?
  fp_json="$(gl_checkpoint_fingerprint_json)" || return $?
  python3 - "${state_file}" "${fp_json}" "${GL_DIR}" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

state_path = Path(sys.argv[1])
new_fp = json.loads(sys.argv[2])
sys.path.insert(0, sys.argv[3])
from _checkpoint_state_io import atomic_write_json

state = json.loads(state_path.read_text(encoding="utf-8"))
if not state.get("fingerprint"):
    state["fingerprint"] = new_fp
    state["updated_at"] = datetime.now(timezone.utc).isoformat()
    atomic_write_json(state_path, state)
PY
}

gl_checkpoint_assert_fingerprint_matches() {
  local state_file fp_json
  state_file="$(gl_checkpoint_state_file)" || return $?
  fp_json="$(gl_checkpoint_fingerprint_json)" || return $?
  python3 - "${state_file}" "${fp_json}" <<'PY'
import json
import os
import stat
import sys
from pathlib import Path

state_path = Path(sys.argv[1])
expected = json.loads(sys.argv[2])
try:
    parent_info = os.lstat(state_path.parent)
    state_info = os.lstat(state_path)
except FileNotFoundError as exc:
    raise SystemExit(f"checkpoint state is not initialized: {state_path}") from exc
if stat.S_ISLNK(parent_info.st_mode) or not stat.S_ISDIR(parent_info.st_mode):
    raise SystemExit(f"checkpoint state directory is unsafe: {state_path.parent}")
if parent_info.st_uid != os.geteuid() or stat.S_IMODE(parent_info.st_mode) & 0o077:
    raise SystemExit(
        f"checkpoint state directory ownership/permissions are unsafe: {state_path.parent}"
    )
if stat.S_ISLNK(state_info.st_mode) or not stat.S_ISREG(state_info.st_mode):
    raise SystemExit(f"checkpoint state file is unsafe: {state_path}")
if state_info.st_uid != os.geteuid() or stat.S_IMODE(state_info.st_mode) & 0o077:
    raise SystemExit(
        f"checkpoint state file ownership/permissions are unsafe: {state_path}"
    )
state = json.loads(state_path.read_text(encoding="utf-8"))
current = state.get("fingerprint") or {}
if current and current != expected:
    diff_keys = sorted(
        key
        for key in set(current.keys()) | set(expected.keys())
        if current.get(key) != expected.get(key)
    )
    print("checkpoint fingerprint mismatch", file=sys.stderr)
    print("state file:", state_path, file=sys.stderr)
    print("changed keys:", ", ".join(diff_keys), file=sys.stderr)
    print("expected:", json.dumps(expected, sort_keys=True), file=sys.stderr)
    print("found:", json.dumps(current, sort_keys=True), file=sys.stderr)
    sys.exit(1)
PY
}

gl_bind_gateway_launch_context() {
  # SYSCOIN: Standalone mutating helpers must serialize with the canonical
  # launcher and bind every deployment-identity input to its persisted run.
  gl_acquire_gateway_launch_lock || return $?
  gl_checkpoint_state_init || return $?
  gl_checkpoint_set_fingerprint_if_empty || return $?
  gl_checkpoint_assert_fingerprint_matches
}

gl_gateway_conversion_deployer_manifest_file() {
  printf '%s/gateway-conversion-deployer.json\n' "$(gl_checkpoint_state_dir)"
}

gl_bind_gateway_conversion_deployer() {
  local deployer="${1:?deployer address required}" manifest_file
  manifest_file="$(gl_gateway_conversion_deployer_manifest_file)" || return $?
  python3 - "${manifest_file}" "${deployer}" "${GL_DIR}" <<'PY'
import json
import os
import re
import stat
import sys
from pathlib import Path

path = Path(sys.argv[1])
deployer = sys.argv[2].strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", deployer) or int(deployer[2:], 16) == 0:
    raise SystemExit("invalid authenticated Gateway deployer address")
sys.path.insert(0, sys.argv[3])
from _checkpoint_state_io import atomic_write_json

expected = {"schema_version": 1, "deployer": deployer}
if path.exists() or path.is_symlink():
    info = os.lstat(path)
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise SystemExit(f"unsafe Gateway conversion deployer manifest: {path}")
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise SystemExit(f"unsafe Gateway conversion deployer manifest ownership/mode: {path}")
    if json.loads(path.read_text(encoding="utf-8")) != expected:
        raise SystemExit("Gateway conversion deployer differs from the first conversion attempt")
else:
    atomic_write_json(path, expected)
PY
}

gl_gateway_conversion_deployer_from_manifest() {
  local manifest_file
  manifest_file="$(gl_gateway_conversion_deployer_manifest_file)" || return $?
  python3 - "${manifest_file}" <<'PY'
import json
import os
import re
import stat
import sys
from pathlib import Path

path = Path(sys.argv[1])
info = os.lstat(path)
if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
    raise SystemExit(f"unsafe Gateway conversion deployer manifest: {path}")
if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077:
    raise SystemExit(f"unsafe Gateway conversion deployer manifest ownership/mode: {path}")
data = json.loads(path.read_text(encoding="utf-8"))
deployer = data.get("deployer") if data.get("schema_version") == 1 else None
if not isinstance(deployer, str) or not re.fullmatch(r"0x[0-9a-f]{40}", deployer) or int(deployer[2:], 16) == 0:
    raise SystemExit(f"invalid Gateway conversion deployer manifest: {path}")
print(deployer)
PY
}

gl_checkpoint_get_status() {
  local checkpoint_id="${1:?checkpoint id required}"
  local state_file
  state_file="$(gl_checkpoint_state_file)" || return $?
  [ -f "${state_file}" ] || {
    printf '%s\n' "pending"
    return 0
  }
  python3 - "${state_file}" "${checkpoint_id}" <<'PY'
import json
import sys
from pathlib import Path

state_path = Path(sys.argv[1])
checkpoint_id = sys.argv[2]
state = json.loads(state_path.read_text(encoding="utf-8"))
entry = (state.get("checkpoints") or {}).get(checkpoint_id) or {}
print(entry.get("status", "pending"))
PY
}

gl_checkpoint_set_status() {
  local checkpoint_id="${1:?checkpoint id required}"
  local status="${2:?status required}"
  local detail="${3:-}"
  local state_file
  state_file="$(gl_checkpoint_state_file)" || return $?
  gl_checkpoint_state_init || return $?
  python3 - "${state_file}" "${checkpoint_id}" "${status}" "${detail}" "${GL_DIR}" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

state_path = Path(sys.argv[1])
checkpoint_id = sys.argv[2]
status = sys.argv[3]
detail = sys.argv[4]
sys.path.insert(0, sys.argv[5])
from _checkpoint_state_io import atomic_write_json

now = datetime.now(timezone.utc).isoformat()

state = json.loads(state_path.read_text(encoding="utf-8"))
checkpoints = state.setdefault("checkpoints", {})
entry = checkpoints.setdefault(checkpoint_id, {})
entry["status"] = status
entry["at"] = now
if detail:
    entry["detail"] = detail
state["current_checkpoint"] = checkpoint_id
state["updated_at"] = now
if status in {"failed", "blocked"}:
    state["last_error"] = {"checkpoint": checkpoint_id, "at": now, "message": detail}
elif (state.get("last_error") or {}).get("checkpoint") == checkpoint_id:
    state["last_error"] = None
atomic_write_json(state_path, state)
PY
}

gl_checkpoint_mark_in_progress() {
  local checkpoint_id="${1:?checkpoint id required}"
  gl_checkpoint_set_status "${checkpoint_id}" "in_progress" ""
}

gl_checkpoint_mark_passed() {
  local checkpoint_id="${1:?checkpoint id required}"
  local detail="${2:-}"
  gl_checkpoint_set_status "${checkpoint_id}" "passed" "${detail}"
}

gl_checkpoint_mark_blocked() {
  local checkpoint_id="${1:?checkpoint id required}"
  local detail="${2:-checkpoint blocked; repair required}"
  gl_checkpoint_set_status "${checkpoint_id}" "blocked" "${detail}"
}

gl_checkpoint_assert_runnable() {
  local checkpoint_id="${1:?checkpoint id required}"
  local status
  status="$(gl_checkpoint_get_status "${checkpoint_id}")" || return $?
  [ "${status}" = "pending" ] ||
    gl_die "checkpoint ${checkpoint_id} status is ${status}; run gateway-launch-repair.sh repair ${checkpoint_id} instead of replaying it automatically"
}

gl_checkpoint_run() {
  local checkpoint_id="${1:?checkpoint id required}"
  shift
  gl_checkpoint_assert_runnable "${checkpoint_id}" || return $?
  gl_checkpoint_mark_in_progress "${checkpoint_id}" || return $?
  if "$@"; then
    gl_checkpoint_mark_passed "${checkpoint_id}" || return $?
  else
    local rc=$?
    gl_checkpoint_mark_blocked "${checkpoint_id}" "command failed with exit code ${rc}" || return $?
    return "${rc}"
  fi
}

gl_checkpoint_mark_repaired() {
  local checkpoint_id="${1:?checkpoint id required}"
  local detail="${2:-repaired and validated}"
  local state_file
  state_file="$(gl_checkpoint_state_file)" || return $?
  gl_checkpoint_mark_passed "${checkpoint_id}" "${detail}" || return $?
  python3 - "${state_file}" "${checkpoint_id}" "${detail}" "${GL_DIR}" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

state_path = Path(sys.argv[1])
checkpoint_id = sys.argv[2]
detail = sys.argv[3]
sys.path.insert(0, sys.argv[4])
from _checkpoint_state_io import atomic_write_json

now = datetime.now(timezone.utc).isoformat()
state = json.loads(state_path.read_text(encoding="utf-8"))
repairs = state.setdefault("repairs", [])
repairs.append({"checkpoint": checkpoint_id, "at": now, "detail": detail})
state["updated_at"] = now
atomic_write_json(state_path, state)
PY
}

# -----------------------------
# Checkpoint probe helpers
# -----------------------------

gl_probe_workspace_ready() {
  gl_require L1_RPC_URL
  gl_require L1_CHAIN_ID
  local rpc_chain_id
  rpc_chain_id="$(gl_l1_chain_id_from_rpc 2>/dev/null || true)"
  [ -n "${rpc_chain_id}" ] &&
    [ -x "${ZKSYNC_ERA_PATH}/zkstack_cli/target/release/zkstack" ] &&
    [ "${rpc_chain_id}" = "${L1_CHAIN_ID}" ]
}

gl_probe_ecosystem_ready() {
  gl_require GATEWAY_DIR
  [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]
}

gl_probe_wallets_funded_ready() {
  gl_require GATEWAY_DIR
  "${GL_DIR}/fund-wallets.sh" --check-only >/dev/null
}

gl_probe_l1_ecosystem_deployed_ready() {
  gl_require GATEWAY_DIR
  gl_require L1_RPC_URL
  local contracts_file resolved bridgehub ctm bytecodes genesis verifier address code registered
  local is_os ctm_bridgehub semver live_genesis live_verifier live_supplier
  local asset_id mapped_ctm asset_router chain_asset_handler deployment_tracker zero_bytes32
  local router_asset_handler router_deployment_tracker stored_batch_zero initial_cut_hash
  contracts_file="${GATEWAY_DIR}/configs/contracts.yaml"
  [ -f "${contracts_file}" ] && [ ! -L "${contracts_file}" ] || return 1
  resolved="$(python3 - "${contracts_file}" <<'PY'
import sys
from pathlib import Path

import yaml


def address(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        parsed = value
    elif isinstance(value, str):
        raw = value.strip()
        if raw.startswith(("0x", "0X")):
            try:
                parsed = int(raw[2:], 16)
            except ValueError:
                raise SystemExit(f"invalid {label}") from None
        elif raw.isdecimal():
            parsed = int(raw, 10)
        else:
            raise SystemExit(f"invalid {label}")
    else:
        raise SystemExit(f"missing {label}")
    if parsed == 0 or parsed >= 1 << 160:
        raise SystemExit(f"invalid {label}")
    return "0x" + format(parsed, "040x")


path = Path(sys.argv[1])
# SYSCOIN: zkstack stores bytecode payloads as unquoted decimal scalars. Keep
# them as text so the Python integer-digit limit cannot block address validation.
data = yaml.load(path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader)
if not isinstance(data, dict):
    raise SystemExit(f"invalid contracts config: {path}")
core = data.get("core_ecosystem_contracts")
zksync_os_ctm = data.get("zksync_os_ctm")
if not isinstance(core, dict) or not isinstance(zksync_os_ctm, dict):
    raise SystemExit(f"missing ecosystem/zkSync OS CTM config: {path}")
print(
    "|".join(
        (
            address(core.get("bridgehub_proxy_addr"), "BridgeHub"),
            address(zksync_os_ctm.get("state_transition_proxy_addr"), "zkSync OS CTM"),
            address(zksync_os_ctm.get("l1_bytecodes_supplier_addr"), "L1 bytecodes supplier"),
            address(zksync_os_ctm.get("genesis_upgrade_addr"), "zkSync OS genesis upgrade"),
            address(zksync_os_ctm.get("verifier_addr"), "zkSync OS verifier"),
        )
    )
)
PY
)" || return $?
  IFS='|' read -r bridgehub ctm bytecodes genesis verifier <<<"${resolved}"
  for address in "${bridgehub}" "${ctm}" "${bytecodes}" "${genesis}" "${verifier}"; do
    code="$(cast code "${address}" --rpc-url "${L1_RPC_URL}")" || return $?
    [ "$(printf '%s' "${code}" | tr -d '[:space:]')" != "0x" ] || return 1
  done
  registered="$(cast call \
    "${bridgehub}" \
    "chainTypeManagerIsRegistered(address)(bool)" \
    "${ctm}" \
    --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(printf '%s' "${registered}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" = "true" ] || return 1
  is_os="$(cast call "${ctm}" "isZKsyncOS()(bool)" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(printf '%s' "${is_os}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" = "true" ] || return 1
  ctm_bridgehub="$(cast call "${ctm}" "BRIDGE_HUB()(address)" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(gl_to_lower "$(printf '%s\n' "${ctm_bridgehub}" | awk 'NF { print $1; exit }')")" = "${bridgehub}" ] || return 1
  semver="$(cast call \
    "${ctm}" \
    "getSemverProtocolVersion()(uint32,uint32,uint32)" \
    --rpc-url "${L1_RPC_URL}")" || return $?
  python3 - "${semver}" <<'PY' || return $?
import re
import sys

values = [int(value) for value in re.findall(r"(?<![0-9])[0-9]+(?![0-9])", sys.argv[1])]
raise SystemExit(0 if values == [0, 32, 0] else 1)
PY
  live_genesis="$(cast call "${ctm}" "l1GenesisUpgrade()(address)" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(gl_to_lower "$(printf '%s\n' "${live_genesis}" | awk 'NF { print $1; exit }')")" = "${genesis}" ] || return 1
  live_supplier="$(cast call "${ctm}" "L1_BYTECODES_SUPPLIER()(address)" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(gl_to_lower "$(printf '%s\n' "${live_supplier}" | awk 'NF { print $1; exit }')")" = "${bytecodes}" ] || return 1
  # Semver 0.32.0 is packed as (major << 64) | (minor << 32) | patch.
  live_verifier="$(cast call \
    "${ctm}" \
    "protocolVersionVerifier(uint256)(address)" \
    137438953472 \
    --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(gl_to_lower "$(printf '%s\n' "${live_verifier}" | awk 'NF { print $1; exit }')")" = "${verifier}" ] || return 1
  zero_bytes32="0x0000000000000000000000000000000000000000000000000000000000000000"
  stored_batch_zero="$(cast call "${ctm}" "storedBatchZero()(bytes32)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  initial_cut_hash="$(cast call "${ctm}" "initialCutHash()(bytes32)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [[ "${stored_batch_zero}" =~ ^0x[0-9a-f]{64}$ ]] &&
    [ "${stored_batch_zero}" != "${zero_bytes32}" ] || return 1
  [[ "${initial_cut_hash}" =~ ^0x[0-9a-f]{64}$ ]] &&
    [ "${initial_cut_hash}" != "${zero_bytes32}" ] || return 1

  # RegisterCTM wires the CTM asset bidirectionally through BridgeHub and the
  # asset router. Validate the complete live relation, not only the first bool.
  asset_id="$(cast call "${bridgehub}" "ctmAssetIdFromAddress(address)(bytes32)" "${ctm}" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [[ "${asset_id}" =~ ^0x[0-9a-f]{64}$ ]] &&
    [ "${asset_id}" != "${zero_bytes32}" ] || return 1
  mapped_ctm="$(cast call "${bridgehub}" "ctmAssetIdToAddress(bytes32)(address)" "${asset_id}" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [ "${mapped_ctm}" = "${ctm}" ] || return 1
  asset_router="$(cast call "${bridgehub}" "assetRouter()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  chain_asset_handler="$(cast call "${bridgehub}" "chainAssetHandler()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  deployment_tracker="$(cast call "${bridgehub}" "l1CtmDeployer()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  for address in "${asset_router}" "${chain_asset_handler}" "${deployment_tracker}"; do
    [[ "${address}" =~ ^0x[0-9a-f]{40}$ ]] &&
      [ "${address}" != "0x0000000000000000000000000000000000000000" ] || return 1
  done
  router_asset_handler="$(cast call "${asset_router}" "assetHandlerAddress(bytes32)(address)" "${asset_id}" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  router_deployment_tracker="$(cast call "${asset_router}" "assetDeploymentTracker(bytes32)(address)" "${asset_id}" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [ "${router_asset_handler}" = "${chain_asset_handler}" ] &&
    [ "${router_deployment_tracker}" = "${deployment_tracker}" ]
}

gl_probe_gateway_chain_inited_ready() {
  gl_require GATEWAY_DIR
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  gl_probe_chain_contracts_schema_ready "${gateway_chain_name}" || return $?
  gl_assert_gateway_chain_config_matches_expected || return $?
  gl_assert_gateway_chain_admin_ready || return $?
  gl_assert_gateway_da_pair_ready
}

gl_probe_gateway_settlement_ready() {
  gl_require GATEWAY_DIR
  gl_require L1_CHAIN_ID
  local gateway_chain_name context bridgehub diamond chain_id settlement_layer whitelisted filterer code
  local expected_context expected_filterer chain_admin chain_proxy_admin governance deployment_tracker deployer conversion_deployer
  local owner pending_owner filterer_bridgehub asset_router filterer_asset_router dangerous value
  local proxy_admin_word live_proxy_admin proxy_admin_code proxy_admin_owner
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  [ -f "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/gateway.yaml" ] || return 1
  gl_probe_gateway_chain_inited_ready || return $?
  gl_assert_gateway_config_identity || return $?
  context="$(gl_gateway_l1_registration_context)" || return $?
  IFS='|' read -r bridgehub diamond <<<"${context}"
  chain_id="$(gl_gateway_chain_id_from_config)" || return $?
  settlement_layer="$(cast call \
    "${bridgehub}" \
    "settlementLayer(uint256)(uint256)" \
    "${chain_id}" \
    --rpc-url "${L1_RPC_URL}" | awk 'NF { print $1; exit }')" || return $?
  [ "${settlement_layer}" = "${L1_CHAIN_ID}" ] || return 1
  whitelisted="$(cast call \
    "${bridgehub}" \
    "whitelistedSettlementLayers(uint256)(bool)" \
    "${chain_id}" \
    --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(printf '%s' "${whitelisted}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" = "true" ] || return 1
  expected_context="$(python3 - \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml" \
    "${GATEWAY_DIR}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

gateway_path, root_path = map(Path, sys.argv[1:])
gateway = yaml.safe_load(gateway_path.read_text(encoding="utf-8"))
# SYSCOIN: Root contracts include very large decimal bytecode scalars; this
# reader only consumes addresses, so parsing all scalars as text is safer.
root = yaml.load(root_path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader)


def address(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        parsed = value
    elif isinstance(value, str):
        raw = value.strip()
        if raw.startswith(("0x", "0X")):
            try:
                parsed = int(raw[2:], 16)
            except ValueError:
                raise SystemExit(f"invalid {label}") from None
        elif raw.isdecimal():
            parsed = int(raw, 10)
        else:
            raise SystemExit(f"invalid {label}")
    else:
        raise SystemExit(f"invalid {label}")
    if parsed == 0 or parsed >= 1 << 160:
        raise SystemExit(f"invalid {label}")
    return "0x" + format(parsed, "040x")


l1 = gateway.get("l1") if isinstance(gateway, dict) else None
ecosystem = gateway.get("ecosystem_contracts") if isinstance(gateway, dict) else None
root_l1 = root.get("l1") if isinstance(root, dict) else None
if not isinstance(l1, dict) or not isinstance(ecosystem, dict) or not isinstance(root_l1, dict):
    raise SystemExit("missing Gateway filterer security context")
print(
    "|".join(
        (
            address(l1.get("transaction_filterer_addr"), "Gateway transaction filterer"),
            address(l1.get("chain_admin_addr"), "Gateway ChainAdmin"),
            address(l1.get("chain_proxy_admin_addr"), "Gateway ProxyAdmin"),
            address(root_l1.get("governance_addr"), "ecosystem governance"),
            address(
                ecosystem.get("stm_deployment_tracker_proxy_addr"),
                "CTM deployment tracker",
            ),
        )
    )
)
PY
)" || return $?
  IFS='|' read -r expected_filterer chain_admin chain_proxy_admin governance deployment_tracker <<<"${expected_context}"
  deployer="$(gl_authenticate_chain_wallet_roles --print-addresses "${gateway_chain_name}" deployer)" || return $?
  deployer="$(gl_to_lower "${deployer}")"
  conversion_deployer="$(gl_gateway_conversion_deployer_from_manifest)" || return $?
  [ "${deployer}" != "${governance}" ] && [ "${deployer}" != "${deployment_tracker}" ] || return 1
  filterer="$(cast call "${diamond}" "getTransactionFilterer()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [[ "${filterer}" =~ ^0x[0-9a-f]{40}$ ]] &&
    [ "${filterer}" != "0x0000000000000000000000000000000000000000" ] || return 1
  [ "${filterer}" = "${expected_filterer}" ] || return 1
  code="$(cast code "${filterer}" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(printf '%s' "${code}" | tr -d '[:space:]')" != "0x" ] || return 1
  proxy_admin_word="$(cast storage \
    "${filterer}" \
    "0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103" \
    --rpc-url "${L1_RPC_URL}")" || return $?
  live_proxy_admin="$(python3 - "${proxy_admin_word}" <<'PY'
import sys

raw = sys.argv[1].strip()
if not raw.startswith(("0x", "0X")) or len(raw) != 66:
    raise SystemExit(1)
value = int(raw[2:], 16)
if value == 0 or value >= 1 << 160:
    raise SystemExit(1)
print("0x" + format(value, "040x"))
PY
)" || return $?
  [ "${live_proxy_admin}" = "${chain_proxy_admin}" ] || return 1
  proxy_admin_code="$(cast code "${chain_proxy_admin}" --rpc-url "${L1_RPC_URL}")" || return $?
  [ "$(printf '%s' "${proxy_admin_code}" | tr -d '[:space:]')" != 0x ] || return 1
  proxy_admin_owner="$(cast call "${chain_proxy_admin}" "owner()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [ "${proxy_admin_owner}" = "${chain_admin}" ] || return 1
  owner="$(cast call "${filterer}" "owner()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  pending_owner="$(cast call "${filterer}" "pendingOwner()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  filterer_bridgehub="$(cast call "${filterer}" "BRIDGE_HUB()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  asset_router="$(cast call "${bridgehub}" "assetRouter()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  filterer_asset_router="$(cast call "${filterer}" "L1_ASSET_ROUTER()(address)" --rpc-url "${L1_RPC_URL}" | awk 'NF { print tolower($1); exit }')" || return $?
  [ "${owner}" = "${chain_admin}" ] &&
    [ "${pending_owner}" = "0x0000000000000000000000000000000000000000" ] &&
    [ "${filterer_bridgehub}" = "${bridgehub}" ] &&
    [ "${filterer_asset_router}" = "${asset_router}" ] || return 1
  dangerous="$(cast call \
    "${filterer}" \
    "dangerousContracts(address)(bool)" \
    "0x4e59b44847b379578588920ca78fbf26c0b4956c" \
    --rpc-url "${L1_RPC_URL}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" || return $?
  [ "${dangerous}" = true ] || return 1
  for value in "${governance}" "${deployment_tracker}"; do
    whitelisted="$(cast call "${filterer}" "whitelistedSenders(address)(bool)" "${value}" --rpc-url "${L1_RPC_URL}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" || return $?
    [ "${whitelisted}" = true ] || return 1
  done
  for value in "${deployer}" "${conversion_deployer}"; do
    whitelisted="$(cast call "${filterer}" "whitelistedSenders(address)(bool)" "${value}" --rpc-url "${L1_RPC_URL}" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')" || return $?
    [ "${whitelisted}" = false ] || return 1
  done
}

gl_probe_os_configs_gateway_ready() {
  gl_require GATEWAY_DIR
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  gl_probe_materialized_os_chain_ready "${gateway_chain_name}" &&
    env MATERIALIZE_EDGE_CONFIG=false \
      "${GL_DIR}/generate-os-server-configs.sh" --check-only >/dev/null
}

gl_probe_materialized_os_chain_ready() {
  gl_require GATEWAY_DIR
  local chain_name="${1:?chain name required}"
  python3 - "${GATEWAY_DIR}/os-server-configs/${chain_name}" <<'PY'
import os
import stat
import sys
from pathlib import Path

root = Path(sys.argv[1])
for name in ("config.yaml", "contracts.yaml", "wallets.yaml", "genesis.json"):
    path = root / name
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        raise SystemExit(1)
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise SystemExit(1)
    if name == "wallets.yaml" and (
        info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o077
    ):
        raise SystemExit(1)
start = root / "start-node.sh"
try:
    info = os.lstat(start)
except FileNotFoundError:
    raise SystemExit(1)
if (
    stat.S_ISLNK(info.st_mode)
    or not stat.S_ISREG(info.st_mode)
    or not os.access(start, os.X_OK)
):
    raise SystemExit(1)
PY
}

gl_probe_edge_chain_inited_ready() {
  gl_require GATEWAY_DIR
  local edge_chain_name
  edge_chain_name="${EDGE_CHAIN_NAME:-zksys}"
  gl_probe_chain_contracts_schema_ready "${edge_chain_name}"
}

gl_probe_edge_chain_inited_and_governor_ready() {
  gl_probe_edge_chain_inited_ready || return $?
  gl_assert_edge_chain_config_matches_expected || return $?
  gl_assert_edge_chain_admin_owned_by_configured_governor
}

# SYSCOIN: Runtime configs sign with private keys from generated wallet YAML.
# Bind each declared operator address to its actual key before treating live
# balances/roles as a valid migration postcondition.
gl_authenticate_chain_wallet_roles() {
  gl_require GATEWAY_DIR
  local emit_addresses=false
  if [ "${1:-}" = "--print-addresses" ]; then
    emit_addresses=true
    shift
  fi
  local chain_name="${1:?chain name required}" cast_bin common_dir
  shift
  [ "$#" -gt 0 ] || gl_die "at least one wallet role is required"
  cast_bin="$(command -v cast || true)"
  if [ -z "${cast_bin}" ] && [ -x "${HOME}/.foundry/bin/cast" ]; then
    cast_bin="${HOME}/.foundry/bin/cast"
  fi
  [ -n "${cast_bin}" ] || gl_die "cast is required to authenticate operator wallets"
  common_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  GL_WALLET_CAST_BIN="${cast_bin}" GL_WALLET_COMMON_DIR="${common_dir}" \
    GL_WALLET_EMIT_ADDRESSES="${emit_addresses}" \
    python3 - \
      "${GATEWAY_DIR}/chains/${chain_name}/configs/wallets.yaml" \
      "${GATEWAY_DIR}/chains/${chain_name}/wallets.yaml" \
      "${GATEWAY_DIR}/configs/wallets.yaml" \
      "$@" <<'PY'
import os
import sys
from pathlib import Path

import yaml

sys.path.insert(0, os.environ["GL_WALLET_COMMON_DIR"])
from _wallet_identity import authenticate_wallet_entry  # noqa: E402

paths = [Path(value) for value in sys.argv[1:4]]
addresses = []
for role in sys.argv[4:]:
    for path in paths:
        if not path.is_file():
            continue
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
        entry = data.get(role) if isinstance(data, dict) else None
        if isinstance(entry, dict):
            address, _ = authenticate_wallet_entry(
                entry, f"{role} in {path}", os.environ["GL_WALLET_CAST_BIN"]
            )
            addresses.append(address)
            break
    else:
        raise SystemExit(f"missing {role} wallet entry")
if os.environ["GL_WALLET_EMIT_ADDRESSES"] == "true":
    print("|".join(addresses))
PY
}

# SYSCOIN: Resolve the Gateway's persisted BridgeHub/diamond pair independently
# of any edge so the chain-init checkpoint has a live, owner-bound postcondition.
gl_gateway_l1_registration_context() {
  gl_require GATEWAY_DIR
  local gateway_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  python3 - \
    "${GATEWAY_DIR}/configs/contracts.yaml" \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path

import yaml


def address(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        parsed = value
    elif isinstance(value, str):
        raw = value.strip()
        if raw.startswith(("0x", "0X")):
            try:
                parsed = int(raw[2:], 16)
            except ValueError:
                raise SystemExit(f"invalid {label}") from None
        elif raw.isdecimal():
            parsed = int(raw, 10)
        else:
            raise SystemExit(f"invalid {label}")
    else:
        raise SystemExit(f"missing {label}")
    if parsed == 0 or parsed >= 1 << 160:
        raise SystemExit(f"invalid {label}")
    return "0x" + format(parsed, "040x")


root_path, chain_path = map(Path, sys.argv[1:])
root = yaml.load(root_path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader)
chain = yaml.safe_load(chain_path.read_text(encoding="utf-8"))
root_core = root.get("core_ecosystem_contracts") if isinstance(root, dict) else None
chain_eco = chain.get("ecosystem_contracts") if isinstance(chain, dict) else None
chain_l1 = chain.get("l1") if isinstance(chain, dict) else None
bridgehub = address(
    root_core.get("bridgehub_proxy_addr") if isinstance(root_core, dict) else None,
    "root BridgeHub",
)
chain_bridgehub = address(
    chain_eco.get("bridgehub_proxy_addr") if isinstance(chain_eco, dict) else None,
    "Gateway BridgeHub",
)
if chain_bridgehub != bridgehub:
    raise SystemExit("Gateway BridgeHub does not match the root ecosystem")
diamond = address(
    chain_l1.get("diamond_proxy_addr") if isinstance(chain_l1, dict) else None,
    "Gateway diamond",
)
print(f"{bridgehub}|{diamond}")
PY
}

gl_assert_gateway_chain_admin_ready() {
  gl_require L1_RPC_URL
  local gateway_chain_name governor context bridgehub diamond chain_id
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  governor="$(gl_authenticate_chain_wallet_roles \
    --print-addresses "${gateway_chain_name}" governor)" || return $?
  context="$(gl_gateway_l1_registration_context)" || return $?
  IFS='|' read -r bridgehub diamond <<<"${context}"
  chain_id="$(gl_gateway_chain_id_from_config)" || return $?
  gl_assert_registered_chain_owned_by_governor \
    "${bridgehub}" "${chain_id}" "${governor}" "Gateway" "${diamond}"
}

gl_assert_gateway_da_pair_ready() {
  gl_require L1_RPC_URL
  local gateway_chain_name context diamond expected raw_pair parsed actual scheme
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  context="$(gl_gateway_l1_registration_context)" || return $?
  diamond="${context#*|}"
  expected="$(python3 - \
    "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8"))
l1 = data.get("l1") if isinstance(data, dict) else None
value = l1.get("rollup_l1_da_validator_addr") if isinstance(l1, dict) else None
if isinstance(value, int) and not isinstance(value, bool):
    value = "0x" + format(value, "040x")
if not isinstance(value, str) or not value.startswith(("0x", "0X")) or len(value) != 42:
    raise SystemExit(f"invalid Gateway compact rollup L1 DA validator in {path}")
parsed = int(value[2:], 16)
if parsed == 0 or parsed >= 1 << 160:
    raise SystemExit(f"invalid Gateway compact rollup L1 DA validator in {path}")
print("0x" + format(parsed, "040x"))
PY
)" || return $?
  raw_pair="$(cast call \
    "${diamond}" \
    "getDAValidatorPair()(address,uint8)" \
    --rpc-url "${L1_RPC_URL}")" || return $?
  parsed="$(python3 - "${raw_pair}" <<'PY'
import re
import sys

raw = sys.argv[1]
match = re.search(r"0x[0-9a-fA-F]{40}", raw)
if match is None:
    raise SystemExit("missing DA validator address")
address = match.group(0).lower()
remainder = raw[: match.start()] + raw[match.end() :]
numbers = [int(value) for value in re.findall(r"(?<![0-9])[0-9]+(?![0-9])", remainder)]
if len(numbers) != 1:
    raise SystemExit("missing DA commitment scheme")
print(f"{address}|{numbers[0]}")
PY
)" || return $?
  IFS='|' read -r actual scheme <<<"${parsed}"
  [ "${actual}" = "${expected}" ] && [ "${scheme}" = "4" ]
}

# SYSCOIN: Resolve the persisted governor identities and authoritative L1
# registration inputs without exposing private keys.
gl_edge_governor_reuse_context() {
  gl_require GATEWAY_DIR
  local gateway_chain_name edge_chain_name cast_bin common_dir
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  edge_chain_name="${EDGE_CHAIN_NAME:-zksys}"
  cast_bin="$(command -v cast || true)"
  if [ -z "${cast_bin}" ] && [ -x "${HOME}/.foundry/bin/cast" ]; then
    cast_bin="${HOME}/.foundry/bin/cast"
  fi
  [ -n "${cast_bin}" ] || gl_die "cast is required to authenticate the Gateway governor key"
  common_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  python3 - \
  "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/wallets.yaml" \
  "${GATEWAY_DIR}/configs/wallets.yaml" \
  "${GATEWAY_DIR}/chains/${edge_chain_name}/configs/wallets.yaml" \
  "${GATEWAY_DIR}/chains/${gateway_chain_name}/ZkStack.yaml" \
  "${GATEWAY_DIR}/chains/${edge_chain_name}/ZkStack.yaml" \
  "${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml" \
  "${GATEWAY_DIR}/chains/${edge_chain_name}/configs/contracts.yaml" \
  "${GATEWAY_DIR}/configs/contracts.yaml" \
  "${GATEWAY_CHAIN_ID:-57001}" \
  "${EDGE_CHAIN_ID:-57057}" \
  "${cast_bin}" \
  "${common_dir}" <<'PY'
import sys
from pathlib import Path

import yaml

sys.path.insert(0, sys.argv[12])
from _wallet_identity import authenticate_wallet_entry  # noqa: E402


def address(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        parsed = value
    elif isinstance(value, str):
        raw = value.strip()
        if raw.startswith(("0x", "0X")):
            try:
                parsed = int(raw[2:], 16)
            except ValueError:
                raise SystemExit(f"invalid {label}: {value}") from None
        elif raw.isdecimal():
            parsed = int(raw, 10)
        else:
            raise SystemExit(f"invalid {label}: {value}")
    else:
        raise SystemExit(f"missing {label}")
    if parsed == 0 or parsed >= 1 << 160:
        raise SystemExit(f"invalid {label}: {value}")
    return "0x" + format(parsed, "040x")


governor = None
for wallet_path in map(Path, sys.argv[1:3]):
    if not wallet_path.exists():
        continue
    wallets = yaml.safe_load(wallet_path.read_text(encoding="utf-8"))
    entry = wallets.get("governor") if isinstance(wallets, dict) else None
    if (
        isinstance(entry, dict)
        and entry.get("address") is not None
        and entry.get("private_key") not in (None, "")
    ):
        governor, _ = authenticate_wallet_entry(
            entry, f"Gateway governor in {wallet_path}", sys.argv[11]
        )
        break
if governor is None:
    raise SystemExit("missing Gateway governor wallet with address/private_key")

edge_wallet_path = Path(sys.argv[3])
wallets = yaml.safe_load(edge_wallet_path.read_text(encoding="utf-8"))
entry = wallets.get("governor") if isinstance(wallets, dict) else None
if (
    not isinstance(entry, dict)
    or entry.get("address") is None
    or entry.get("private_key") in (None, "")
):
    raise SystemExit(f"invalid edge governor wallet entry in {edge_wallet_path}")
edge_governor, _ = authenticate_wallet_entry(
    entry, f"edge governor in {edge_wallet_path}", sys.argv[11]
)
edge_governor_matches = edge_governor == governor

def chain_id(path, label, expected_raw):
    chain = yaml.safe_load(path.read_text(encoding="utf-8"))
    value = chain.get("chain_id") if isinstance(chain, dict) else None
    if isinstance(value, str):
        value = int(value, 16 if value.lower().startswith("0x") else 10)
    if isinstance(value, bool) or not isinstance(value, int) or not 0 < value < 2**32:
        raise SystemExit(f"invalid {label} chain_id in {path}")
    expected = int(expected_raw, 10)
    if value != expected:
        raise SystemExit(
            f"{label} chain_id mismatch: configured={expected} persisted={value}"
        )
    return value


gateway_chain_id = chain_id(Path(sys.argv[4]), "Gateway", sys.argv[9])
edge_chain_id = chain_id(Path(sys.argv[5]), "edge", sys.argv[10])

contracts_path = Path(sys.argv[8])
contracts = yaml.load(
    contracts_path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader
)
core = contracts.get("core_ecosystem_contracts") if isinstance(contracts, dict) else None
bridgehub = core.get("bridgehub_proxy_addr") if isinstance(core, dict) else None
bridgehub = address(bridgehub, "L1 BridgeHub")


def chain_contract_identity(path, label, required):
    if not path.is_file():
        if required:
            raise SystemExit(f"missing {label} contracts config: {path}")
        return ""
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit(f"invalid {label} contracts config: {path}")
    ecosystem = data.get("ecosystem_contracts")
    local_bridgehub = (
        ecosystem.get("bridgehub_proxy_addr")
        if isinstance(ecosystem, dict)
        else None
    )
    local_bridgehub = address(local_bridgehub, f"{label} L1 BridgeHub")
    if local_bridgehub != bridgehub:
        raise SystemExit(
            f"{label} BridgeHub mismatch: ecosystem={bridgehub} "
            f"chain={local_bridgehub}"
        )
    l1 = data.get("l1")
    raw_diamond = l1.get("diamond_proxy_addr") if isinstance(l1, dict) else None
    if raw_diamond in (None, ""):
        diamond = ""
    else:
        if isinstance(raw_diamond, int) and not isinstance(raw_diamond, bool):
            raw_diamond = "0x" + format(raw_diamond, "040x")
        if not isinstance(raw_diamond, str):
            raise SystemExit(f"invalid {label} diamond_proxy_addr in {path}")
        raw_diamond = raw_diamond.strip()
        if not raw_diamond.startswith(("0x", "0X")) or len(raw_diamond) != 42:
            raise SystemExit(f"invalid {label} diamond_proxy_addr in {path}")
        value = int(raw_diamond[2:], 16)
        diamond = "" if value == 0 else "0x" + format(value, "040x")
    if required and not diamond:
        raise SystemExit(f"missing nonzero {label} l1.diamond_proxy_addr in {path}")
    return diamond


gateway_diamond = chain_contract_identity(Path(sys.argv[6]), "Gateway", True)
edge_diamond = chain_contract_identity(Path(sys.argv[7]), "edge", False)
print(
    f"{governor}|{edge_governor}|{str(edge_governor_matches).lower()}|"
    f"{gateway_chain_id}|{edge_chain_id}|{bridgehub}|"
    f"{gateway_diamond}|{edge_diamond}"
)
PY
}

gl_normalize_cast_address() {
  local label="${1:?label required}" raw="${2:-}" normalized
  normalized="$(printf '%s\n' "${raw}" | awk 'NF { print tolower($1); exit }')"
  [[ "${normalized}" =~ ^0x[0-9a-f]{40}$ ]] || \
    gl_die "invalid ${label} address returned by L1: ${raw:-<empty>}"
  printf '%s\n' "${normalized}"
}

gl_registered_chain_admin() {
  local bridgehub="${1:?BridgeHub required}" chain_id="${2:?chain ID required}"
  local label="${3:?chain label required}"
  local expected_diamond="${4:-}"
  local raw_diamond diamond raw_chain_admin chain_admin
  raw_diamond="$(cast call "${bridgehub}" "getZKChain(uint256)(address)" "${chain_id}" --rpc-url "${L1_RPC_URL}")" || \
    gl_die "failed to query L1 BridgeHub registration for ${label} chain ${chain_id}"
  diamond="$(gl_normalize_cast_address "${label} diamond" "${raw_diamond}")" || return $?
  if [ "${diamond}" = "0x0000000000000000000000000000000000000000" ]; then
    [ -z "${expected_diamond}" ] ||
      gl_die "persisted ${label} diamond ${expected_diamond} is not registered in BridgeHub ${bridgehub}"
    return 0
  fi
  [ -n "${expected_diamond}" ] ||
    gl_die "registered ${label} chain ${chain_id} is missing a persisted diamond identity"
  [ "${diamond}" = "${expected_diamond}" ] ||
    gl_die "${label} diamond mismatch: persisted=${expected_diamond} registered=${diamond}"
  raw_chain_admin="$(cast call "${diamond}" "getAdmin()(address)" --rpc-url "${L1_RPC_URL}")" || \
    gl_die "failed to read ChainAdmin for registered ${label} diamond ${diamond}"
  chain_admin="$(gl_normalize_cast_address "${label} ChainAdmin" "${raw_chain_admin}")" || return $?
  [ "${chain_admin}" != "0x0000000000000000000000000000000000000000" ] || \
    gl_die "registered ${label} diamond ${diamond} returned a zero ChainAdmin"
  printf '%s\n' "${chain_admin}"
}

gl_assert_registered_chain_owned_by_governor() {
  local bridgehub="${1:?BridgeHub required}" chain_id="${2:?chain ID required}"
  local expected_governor="${3:?expected governor required}" label="${4:?chain label required}"
  local expected_diamond="${5:?expected diamond required}"
  local chain_admin
  chain_admin="$(gl_registered_chain_admin "${bridgehub}" "${chain_id}" "${label}" "${expected_diamond}")" || return $?
  [ -n "${chain_admin}" ] || gl_die "${label} chain ${chain_id} is not registered on L1"
  gl_assert_chain_admin_owner "${chain_admin}" "${expected_governor}" "${label}"
}

gl_assert_chain_admin_owner() {
  local chain_admin="${1:?chain admin required}"
  local expected_governor="${2:?expected governor required}"
  local label="${3:-edge}"
  local chain_admin_code actual_governor

  chain_admin_code="$(cast code "${chain_admin}" --rpc-url "${L1_RPC_URL}")" || \
    gl_die "failed to read ${label} ChainAdmin runtime at ${chain_admin}"
  if [ "$(printf '%s' "${chain_admin_code}" | tr -d '[:space:]')" = "0x" ]; then
    gl_die "missing ${label} ChainAdmin runtime at ${chain_admin}"
  fi
  actual_governor="$(cast call "${chain_admin}" "owner()(address)" --rpc-url "${L1_RPC_URL}")" || \
    gl_die "failed to read owner of ${label} ChainAdmin ${chain_admin}"
  actual_governor="$(printf '%s\n' "${actual_governor}" | awk 'NF { print $1; exit }')"
  if [ "$(gl_to_lower "${actual_governor}")" != "${expected_governor}" ]; then
    gl_die "${label} ChainAdmin owner mismatch: expected ${expected_governor}, got ${actual_governor:-<empty>}"
  fi
}

# Fail before replacing an existing edge governor key unless live L1 state or
# this invocation proves that no different governor can control the edge.
gl_assert_existing_edge_chain_admin_safe_for_governor_reuse() {
  gl_require L1_RPC_URL
  local edge_chain_created="${1:?edge-created flag required}"
  local resolved expected_governor edge_governor edge_governor_matches gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond chain_admin
  resolved="$(gl_edge_governor_reuse_context)" || return $?
  IFS='|' read -r expected_governor edge_governor edge_governor_matches gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond <<<"${resolved}"
  gl_assert_registered_chain_owned_by_governor \
    "${bridgehub}" "${gateway_chain_id}" "${expected_governor}" "Gateway" "${gateway_diamond}" || return $?
  chain_admin="$(gl_registered_chain_admin "${bridgehub}" "${edge_chain_id}" "edge" "${edge_diamond}")" || return $?
  if [ -n "${chain_admin}" ]; then
    gl_assert_chain_admin_owner "${chain_admin}" "${expected_governor}"
    return
  fi
  if [ "${edge_chain_created}" = "true" ] || [ "${edge_governor_matches}" = "true" ]; then
    return 0
  fi
  gl_die "edge chain ${edge_chain_id} exists locally but is not registered on L1 and its governor differs from the Gateway governor; refusing to overwrite an ambiguously controlling key"
}

gl_assert_edge_chain_admin_owned_by_gateway_governor() {
  EDGE_REUSE_GATEWAY_GOVERNOR=true \
    gl_assert_edge_chain_admin_owned_by_configured_governor
}

# SYSCOIN: Cross-bind both persisted diamonds to BridgeHub and authenticate the
# private key behind whichever governor policy owns the edge. Gateway identity
# is unconditional; disabling governor reuse changes only the expected edge
# owner, never the live-registration checks.
gl_assert_edge_chain_admin_owned_by_configured_governor() {
  gl_require L1_RPC_URL
  local resolved gateway_governor edge_governor edge_governor_matches gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond chain_admin expected_edge_governor reuse_governor
  resolved="$(gl_edge_governor_reuse_context)" || return $?
  IFS='|' read -r gateway_governor edge_governor edge_governor_matches gateway_chain_id edge_chain_id bridgehub gateway_diamond edge_diamond <<<"${resolved}"
  gl_assert_registered_chain_owned_by_governor \
    "${bridgehub}" "${gateway_chain_id}" "${gateway_governor}" "Gateway" "${gateway_diamond}" || return $?
  reuse_governor="$(gl_to_lower "${EDGE_REUSE_GATEWAY_GOVERNOR:-true}")"
  case "${reuse_governor}" in
  true)
    [ "${edge_governor_matches}" = "true" ] ||
      gl_die "edge governor wallet does not contain the authenticated Gateway governor"
    expected_edge_governor="${gateway_governor}"
    ;;
  false) expected_edge_governor="${edge_governor}" ;;
  *) gl_die "EDGE_REUSE_GATEWAY_GOVERNOR must be true or false" ;;
  esac
  chain_admin="$(gl_registered_chain_admin "${bridgehub}" "${edge_chain_id}" "edge" "${edge_diamond}")" || return $?
  [ -n "${chain_admin}" ] || \
    gl_die "edge chain ${edge_chain_id} is still unregistered on L1 after init"
  gl_assert_chain_admin_owner "${chain_admin}" "${expected_edge_governor}" "edge"
}

gl_probe_os_configs_final_ready() {
  gl_require GATEWAY_DIR
  local gateway_chain_name edge_chain_name
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  edge_chain_name="${EDGE_CHAIN_NAME:-zksys}"
  gl_probe_materialized_os_chain_ready "${gateway_chain_name}" &&
    gl_probe_materialized_os_chain_ready "${edge_chain_name}" &&
    "${GL_DIR}/generate-os-server-configs.sh" --check-only >/dev/null
}

gl_probe_chain_contracts_schema_ready() {
  gl_require GATEWAY_DIR
  local chain_name="${1:?chain name required}"
  local contracts_yaml
  contracts_yaml="${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml"
  [ -f "${contracts_yaml}" ] || return 1

  # SYSCOIN: retain the typed/raw DA authentication before reading zkstack's
  # unquoted canonical 0x scalars without PyYAML 1.1 integer coercion.
  gl_assert_chain_contracts_da_preinit_safe "${chain_name}" >/dev/null 2>&1 || return 1

  python3 - "${contracts_yaml}" <<'PY'
import re
import sys
from pathlib import Path

import yaml

p = Path(sys.argv[1])
# SYSCOIN: the typed/raw preflight above rejects tag/type ambiguity; BaseLoader
# preserves the authenticated spelling of zkstack's unquoted 0x scalars.
data = yaml.load(p.read_text(encoding="utf-8"), Loader=yaml.BaseLoader)
if not isinstance(data, dict):
    raise SystemExit(1)

l2 = data.get("l2")
if not isinstance(l2, dict):
    raise SystemExit(1)

required_top = ("create2_factory_addr", "create2_factory_salt")
for key in required_top:
    if key not in data:
        raise SystemExit(1)

required_l2 = ("default_l2_upgrader", "da_validator_addr")
for key in required_l2:
    if key not in l2:
        raise SystemExit(1)

def canonical_nonzero_address(value):
    if not isinstance(value, str) or re.fullmatch(r"0x[0-9a-f]{40}", value) is None:
        raise SystemExit(1)
    if value == "0x0000000000000000000000000000000000000000":
        raise SystemExit(1)
    return value

# SYSCOIN: Readiness means both compact-rollup scopes are authenticated and all
# unsupported chain-local L1 DA slots remain the canonical zero sentinel.
eco = data.get("ecosystem_contracts")
l1 = data.get("l1")
if not isinstance(eco, dict) or not isinstance(l1, dict):
    raise SystemExit(1)
zero = "0x0000000000000000000000000000000000000000"
canonical_nonzero_address(eco.get("rollup_l1_da_validator_addr"))
canonical_nonzero_address(l1.get("rollup_l1_da_validator_addr"))
if any(
    l1.get(field) != zero
    for field in (
        "blobs_zksync_os_l1_da_validator_addr",
        "no_da_validium_l1_validator_addr",
        "avail_l1_da_validator_addr",
    )
):
    raise SystemExit(1)
PY
}
