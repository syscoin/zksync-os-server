#!/usr/bin/env bash
# Create and zkstack-init an edge (child) chain under the ecosystem (§5).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
RESUME_CREATED_ONLY=false
RESUME_POST_ADMIN=false
case "$#:${1:-}" in
0:) ;;
1:--resume-created-only) RESUME_CREATED_ONLY=true ;;
1:--resume-post-admin) RESUME_POST_ADMIN=true ;;
*) gl_die "usage: edge-chain-create-init.sh [--resume-created-only|--resume-post-admin]" ;;
esac
gl_require ZKSYNC_ERA_PATH
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
: "${EDGE_CHAIN_NAME:=zksys}"
gl_validate_zkstack_chain_name "${EDGE_CHAIN_NAME}" EDGE_CHAIN_NAME
# SYSCOIN: Initialize only the canonical fresh V32 lane.
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
gl_assert_zksync_era_sha
gl_ensure_zkstack_cli_release_current
gl_path_for_zkstack
gl_export_foundry_evm_version
: "${GATEWAY_DIR:=${HOME}/gateway}"
cd "${GATEWAY_DIR}"

if [ -z "${EDGE_CHAIN_ID:-}" ]; then
  [ "${EDGE_CHAIN_NAME}" = "zksys" ] ||
    gl_die "EDGE_CHAIN_ID is required for non-default edge ${EDGE_CHAIN_NAME}"
  EDGE_CHAIN_ID=57057
fi
: "${EDGE_PROVER_MODE:=}"
: "${EDGE_WALLET_CREATION:=}"
: "${EDGE_WALLET_PATH:=${GATEWAY_DIR}/.${EDGE_CHAIN_NAME}-wallets.yaml}"
: "${EDGE_REUSE_GATEWAY_GOVERNOR:=true}"
EDGE_REUSE_GATEWAY_GOVERNOR="$(gl_to_lower "${EDGE_REUSE_GATEWAY_GOVERNOR}")"
case "${EDGE_REUSE_GATEWAY_GOVERNOR}" in
true | false) ;;
*) gl_die "EDGE_REUSE_GATEWAY_GOVERNOR must be true or false" ;;
esac
if [ -z "${SKIP_FUND:-}" ]; then
  SKIP_FUND=false
fi
: "${MIGRATE_EDGE:=false}"
MIGRATE_EDGE="$(gl_to_lower "${MIGRATE_EDGE}")"
case "${MIGRATE_EDGE}" in true | false) ;; *) gl_die "MIGRATE_EDGE must be true or false" ;; esac
export MIGRATE_EDGE

if [ -z "${EDGE_WALLET_CREATION}" ]; then
  EDGE_WALLET_CREATION="$(gl_wallet_creation_for_path "${EDGE_WALLET_PATH}")"
fi

if [ -z "${EDGE_PROVER_MODE}" ]; then
  if [ "${PROVER_MODE}" = "no-proofs" ]; then
    EDGE_PROVER_MODE="no-proofs"
  else
    EDGE_PROVER_MODE="gpu"
  fi
fi
gl_normalize_canonical_deployment_inputs
gl_reject_no_proofs_on_mainnet
gl_validate_l1_network_pair
if [ "${RESUME_CREATED_ONLY}" = true ] || [ "${RESUME_POST_ADMIN}" = true ]; then
  # SYSCOIN: Repair must inherit an existing checkpoint identity; it may not
  # manufacture a new launch context from the recovery invocation's inputs.
  gl_acquire_gateway_launch_lock
  gl_assert_edge_launch_context
else
  gl_bind_edge_launch_context
fi
gl_l1_broadcast_preflight

if [ "${RESUME_POST_ADMIN}" = true ]; then
  [ "${GATEWAY_EDGE_POST_ADMIN_REPAIR:-false}" = true ] || \
    gl_die "--resume-post-admin is internal to gateway-launch-repair.sh"
  # SYSCOIN: This schema-only recovery needs persisted config and authenticated
  # L1 state, never a live Gateway node or a broadcast-capable command.
  gl_assert_edge_post_admin_resume_safe
  gl_secure_generated_secret_file \
    "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/secrets.yaml" \
    "generated edge secrets file"
  gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"
  gl_probe_edge_chain_inited_ready
  gl_assert_edge_post_admin_resume_safe
  exit 0
fi

# SYSCOIN: This helper is also used directly for additional edges and by the
# repair command. Authenticate the configured and live Gateway before any edge
# wallet is consumed or chain-creation transaction can be broadcast.
gl_assert_gateway_runtime_identity

if [ "${RESUME_CREATED_ONLY}" = true ]; then
  [ "${GATEWAY_EDGE_CREATED_ONLY_REPAIR:-false}" = true ] || \
    gl_die "--resume-created-only is internal to gateway-launch-repair.sh"
  gl_assert_edge_created_only_resume_safe
fi

if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  gl_require EDGE_WALLET_PATH
  gl_prepare_wallet_file_for_in_file "${EDGE_WALLET_PATH}"
fi

wallet_args=(--wallet-creation "${EDGE_WALLET_CREATION}")
if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  wallet_args+=(--wallet-path "${EDGE_WALLET_PATH}")
fi

edge_chain_created=false
if [ "${RESUME_CREATED_ONLY}" = true ]; then
  edge_chain_created=true
  echo "gateway-launch: resuming exact created-only edge ${EDGE_CHAIN_NAME}"
elif [ -f "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/ZkStack.yaml" ]; then
  echo "gateway-launch: edge chain ${EDGE_CHAIN_NAME} already exists; skipping chain create"
else
  gl_zkstack_private_pty zkstack chain create \
    --chain-name "${EDGE_CHAIN_NAME}" \
    --chain-id "${EDGE_CHAIN_ID}" \
    --prover-mode "${EDGE_PROVER_MODE}" \
    "${wallet_args[@]}" \
    --l1-batch-commit-data-generator-mode rollup \
    --base-token-address 0x0000000000000000000000000000000000000001 \
    --base-token-price-nominator 1 \
    --base-token-price-denominator 1 \
    --set-as-default false \
    --evm-emulator false \
    --zksync-os
  edge_chain_created=true
fi

if [ -f "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" ]; then
  gl_secure_generated_wallet_file "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml"
fi
gl_assert_edge_chain_config_matches_expected

if [ "${EDGE_REUSE_GATEWAY_GOVERNOR}" = "true" ]; then
  gl_assert_existing_edge_chain_admin_safe_for_governor_reuse "${edge_chain_created}"
  wallet_identity_cast_bin="$(command -v cast || true)"
  if [ -z "${wallet_identity_cast_bin}" ] && [ -x "${HOME}/.foundry/bin/cast" ]; then
    wallet_identity_cast_bin="${HOME}/.foundry/bin/cast"
  fi
  [ -n "${wallet_identity_cast_bin}" ] || gl_die "cast is required to authenticate the Gateway governor key"
  python3 - \
    "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME:-gateway}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" \
    "${EDGE_WALLET_PATH}" \
    "${wallet_identity_cast_bin}" \
    "${SCRIPT_DIR}" <<'PY'
import os
import sys
import tempfile
from pathlib import Path

import yaml

sys.path.insert(0, sys.argv[6])
from _wallet_identity import (  # noqa: E402
    authenticate_wallet_entry,
    normalize_address,
    normalize_private_key,
)

gateway_wallet_paths = [Path(sys.argv[1]), Path(sys.argv[2])]
edge_wallet_paths = [Path(sys.argv[3]), Path(sys.argv[4])]
cast_bin = sys.argv[5]


def normalize_wallet_hex_fields(data):
    if not isinstance(data, dict):
        return data
    for wallet_name, wallet in data.items():
        if not isinstance(wallet, dict):
            continue
        if "address" in wallet:
            wallet["address"] = normalize_address(
                wallet["address"], f"{wallet_name} address"
            )
        if "private_key" in wallet:
            wallet["private_key"] = normalize_private_key(
                wallet["private_key"], f"{wallet_name} private key"
            )
    return data


def validate_wallet_target(path):
    try:
        st = os.lstat(path)
    except FileNotFoundError as exc:
        raise SystemExit(f"edge wallet file disappeared before rewrite: {path}") from exc
    if not os.path.isfile(path) or os.path.islink(path):
        raise SystemExit(f"edge wallet file must be a regular non-symlink file: {path}")
    if st.st_uid != os.geteuid():
        raise SystemExit(f"edge wallet file must be owned by the launching user: {path}")
    if st.st_mode & 0o022:
        raise SystemExit(f"edge wallet file must not be writable by group/other users: {path}")

    parent = path.parent
    parent_st = os.lstat(parent)
    if not os.path.isdir(parent) or os.path.islink(parent):
        raise SystemExit(f"edge wallet parent must be a regular directory: {parent}")
    if parent_st.st_uid != os.geteuid():
        raise SystemExit(f"edge wallet parent must be owned by the launching user: {parent}")


def write_wallet_file_securely(path, data):
    validate_wallet_target(path)
    text = yaml.safe_dump(data, sort_keys=False)
    fd = None
    tmp_name = None
    try:
        fd, tmp_name = tempfile.mkstemp(
            prefix=f".{path.name}.",
            suffix=".tmp",
            dir=path.parent,
            text=True,
        )
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as tmp:
            fd = None
            tmp.write(text)
            tmp.flush()
            os.fsync(tmp.fileno())
        os.replace(tmp_name, path)
        os.chmod(path, 0o600)
    except BaseException:
        if fd is not None:
            os.close(fd)
        if tmp_name is not None:
            try:
                os.unlink(tmp_name)
            except FileNotFoundError:
                pass
        raise


gateway_governor = None
gateway_source = None
for path in gateway_wallet_paths:
    if not path.exists():
        continue
    data = normalize_wallet_hex_fields(
        yaml.safe_load(path.read_text(encoding="utf-8"))
    )
    if not isinstance(data, dict):
        continue
    governor = data.get("governor")
    if (
        isinstance(governor, dict)
        and governor.get("address") is not None
        and governor.get("private_key") not in (None, "")
    ):
        gateway_governor = dict(governor)
        address, private_key = authenticate_wallet_entry(
            governor, f"Gateway governor in {path}", cast_bin
        )
        gateway_governor["address"] = address
        gateway_governor["private_key"] = private_key
        gateway_source = path
        break

if gateway_governor is None:
    raise SystemExit(
        "missing Gateway governor wallet entry with address/private_key; "
        "set EDGE_REUSE_GATEWAY_GOVERNOR=false to keep a separately generated edge governor"
    )

updated = []
seen = set()
for edge_wallet_path in edge_wallet_paths:
    identity = os.path.abspath(edge_wallet_path)
    if identity in seen or not edge_wallet_path.exists():
        continue
    seen.add(identity)
    data = normalize_wallet_hex_fields(
        yaml.safe_load(edge_wallet_path.read_text(encoding="utf-8"))
    )
    if not isinstance(data, dict) or not isinstance(data.get("governor"), dict):
        raise SystemExit(f"invalid edge governor wallet entry in {edge_wallet_path}")
    data["governor"] = dict(gateway_governor)
    write_wallet_file_securely(edge_wallet_path, data)
    updated.append(edge_wallet_path)

if not updated:
    raise SystemExit("no edge wallet files found to update with Gateway governor")

print(
    f"gateway-launch: reused Gateway governor {gateway_governor['address']} "
    f"from {gateway_source} for edge wallets: "
    + ", ".join(str(path) for path in updated)
)
PY
fi

if [ "${EDGE_WALLET_CREATION}" = "random" ] &&
  [ ! -e "${EDGE_WALLET_PATH}" ] && [ ! -L "${EDGE_WALLET_PATH}" ]; then
  # SYSCOIN: Persist on a resume too; chain creation may have completed before
  # an interruption in the governor-authentication window.
  gl_persist_wallet_file "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" "${EDGE_WALLET_PATH}"
  echo "gateway-launch: persisted edge wallets to ${EDGE_WALLET_PATH}"
fi

if [ "${SKIP_FUND}" != "true" ]; then
  GATEWAY_FUND_EDGE_CONTEXT=true \
    GATEWAY_FUND_TARGET_CHAIN_NAME="${EDGE_CHAIN_NAME}" \
    "${SCRIPT_DIR}/fund-wallets.sh"
else
  echo "gateway-launch: SKIP_FUND=true, skipping edge wallet funding"
fi

if [ "${RESUME_CREATED_ONLY}" = true ]; then
  # SYSCOIN: Funding confirmation may take minutes. Re-attest the zero
  # registration and synchronized governor nonce immediately before init.
  gl_assert_edge_created_only_resume_safe
fi

init_args=(
  zkstack chain init
  --chain "${EDGE_CHAIN_NAME}"
  --no-genesis
  --deploy-paymaster false
  --skip-priority-txs
)
if [ "$(gl_to_lower "${MIGRATE_EDGE:-false}")" = true ]; then
  # SYSCOIN: Immediate Gateway migration begins paused; avoid an unnecessary
  # unpause/re-pause transaction pair between init and migration.
  init_args+=(--pause-deposits)
fi
init_args+=(--l1-rpc-url "${L1_RPC_URL}")

init_output=""
if ! init_output="$(gl_zkstack_private_pty "${init_args[@]}" 2>&1)"; then
  init_output_lc="$(gl_to_lower "${init_output}")"
  echo "${init_output}"
  case "${init_output_lc}" in
  *"already initialized"* | *"already deployed"* | *"already exists"*)
    echo "gateway-launch: edge chain ${EDGE_CHAIN_NAME} is already initialized; continuing"
    ;;
  *)
    exit 1
    ;;
  esac
else
  echo "${init_output}"
fi

gl_ensure_chain_contracts_yaml_schema "${EDGE_CHAIN_NAME}"

# SYSCOIN: Wallet replacement must survive a resume after `chain create`, and
# every governor policy must bind its authenticated key and persisted diamond
# to the live L1 BridgeHub registration.
gl_probe_edge_chain_inited_and_governor_ready
