#!/usr/bin/env bash
# Create and zkstack-init an edge (child) chain under the ecosystem (§5).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require ZKSYNC_ERA_PATH
gl_require L1_RPC_URL
: "${PROTOCOL_VERSION:=v31.0}"
export REQUIRED_ZKSTACK_CLI_SHA="${REQUIRED_ZKSTACK_CLI_SHA:-$(gl_zkstack_cli_sha_from_versions)}"
gl_assert_zksync_era_sha
gl_path_for_zkstack
gl_export_foundry_evm_version
: "${GATEWAY_DIR:=${HOME}/gateway}"
cd "${GATEWAY_DIR}"

: "${EDGE_CHAIN_NAME:=zksys}"
: "${EDGE_CHAIN_ID:=57057}"
: "${EDGE_PROVER_MODE:=}"
: "${EDGE_WALLET_CREATION:=}"
: "${EDGE_WALLET_PATH:=${GATEWAY_DIR}/.${EDGE_CHAIN_NAME}-wallets.yaml}"
: "${EDGE_REUSE_GATEWAY_GOVERNOR:=true}"
if [ -z "${SKIP_FUND:-}" ]; then
  SKIP_FUND=false
fi

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
gl_reject_no_proofs_on_mainnet

if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  gl_require EDGE_WALLET_PATH
  gl_prepare_wallet_file_for_in_file "${EDGE_WALLET_PATH}"
fi

wallet_args=(--wallet-creation "${EDGE_WALLET_CREATION}")
if [ "${EDGE_WALLET_CREATION}" = "in-file" ]; then
  wallet_args+=(--wallet-path "${EDGE_WALLET_PATH}")
fi

edge_chain_created=false
if [ -f "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/ZkStack.yaml" ]; then
  echo "gateway-launch: edge chain ${EDGE_CHAIN_NAME} already exists; skipping chain create"
else
  gl_zkstack_pty zkstack chain create \
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

  if [ "${EDGE_WALLET_CREATION}" = "random" ] && [ ! -e "${EDGE_WALLET_PATH}" ] && [ ! -L "${EDGE_WALLET_PATH}" ]; then
    gl_persist_wallet_file "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" "${EDGE_WALLET_PATH}"
    echo "gateway-launch: persisted edge wallets to ${EDGE_WALLET_PATH}"
  fi
fi

if [ -f "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" ]; then
  gl_secure_generated_wallet_file "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml"
fi

if [ "${edge_chain_created}" = true ] && [ "$(gl_to_lower "${EDGE_REUSE_GATEWAY_GOVERNOR}")" = "true" ]; then
  python3 - \
    "${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME:-gateway}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/configs/wallets.yaml" \
    "${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/wallets.yaml" \
    "${EDGE_WALLET_PATH}" <<'PY'
import os
import sys
import tempfile
from pathlib import Path

import yaml

gateway_wallet_paths = [Path(sys.argv[1]), Path(sys.argv[2])]
edge_wallet_paths = [Path(sys.argv[3]), Path(sys.argv[4])]


def hex_string(value, bytes_len):
    if isinstance(value, int):
        return "0x" + format(value & ((1 << (8 * bytes_len)) - 1), f"0{bytes_len * 2}x")
    if isinstance(value, str):
        stripped = value.strip()
        if stripped.startswith(("0x", "0X")):
            return "0x" + stripped[2:].zfill(bytes_len * 2).lower()
        if stripped.isdecimal():
            return "0x" + format(int(stripped, 10), f"0{bytes_len * 2}x")
        return stripped
    return value


def normalize_wallet_hex_fields(data):
    if not isinstance(data, dict):
        return data
    for wallet in data.values():
        if not isinstance(wallet, dict):
            continue
        if "address" in wallet:
            wallet["address"] = hex_string(wallet["address"], 20)
        if "private_key" in wallet:
            wallet["private_key"] = hex_string(wallet["private_key"], 32)
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
    data = normalize_wallet_hex_fields(yaml.safe_load(path.read_text(encoding="utf-8")))
    if not isinstance(data, dict):
        continue
    governor = data.get("governor")
    if isinstance(governor, dict) and governor.get("address") is not None and governor.get("private_key") is not None:
        gateway_governor = dict(governor)
        gateway_source = path
        break

if gateway_governor is None:
    raise SystemExit(
        "missing Gateway governor wallet entry with address/private_key; "
        "set EDGE_REUSE_GATEWAY_GOVERNOR=false to keep a separately generated edge governor"
    )

updated = []
for path in edge_wallet_paths:
    if not path.exists():
        continue
    data = normalize_wallet_hex_fields(yaml.safe_load(path.read_text(encoding="utf-8")))
    if not isinstance(data, dict) or not isinstance(data.get("governor"), dict):
        raise SystemExit(f"invalid edge governor wallet entry in {path}")
    data["governor"] = dict(gateway_governor)
    write_wallet_file_securely(path, data)
    updated.append(path)

if not updated:
    raise SystemExit("no edge wallet files found to update with Gateway governor")

address = gateway_governor["address"]
if isinstance(address, int):
    address = "0x" + format(address & ((1 << 160) - 1), "040x")
print(
    f"gateway-launch: reused Gateway governor {address} from {gateway_source} "
    f"for edge wallets: {', '.join(str(p) for p in updated)}"
)
PY
fi

if [ "${SKIP_FUND}" != "true" ]; then
  GATEWAY_CHAIN_NAME="${EDGE_CHAIN_NAME}" "${SCRIPT_DIR}/fund-wallets.sh"
else
  echo "gateway-launch: SKIP_FUND=true, skipping edge wallet funding"
fi

init_output=""
if ! init_output="$(gl_zkstack_pty zkstack chain init \
  --chain "${EDGE_CHAIN_NAME}" \
  --no-genesis \
  --deploy-paymaster false \
  --skip-priority-txs \
  --l1-rpc-url "${L1_RPC_URL}" 2>&1)"; then
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
