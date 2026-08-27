#!/usr/bin/env bash
# SYSCOIN: Shared Gateway execute-operator nonce lock. This file is meant to be sourced.

if [ "${SYSCOIN_EXECUTE_OPERATOR_LOCK_LIBRARY_LOADED:-false}" = "true" ]; then
  return 0
fi
SYSCOIN_EXECUTE_OPERATOR_LOCK_LIBRARY_LOADED=true

readonly GATEWAY_EXECUTE_OPERATOR_LOCK_FD=9
GATEWAY_EXECUTE_OPERATOR_LOCK_KEY=""
GATEWAY_EXECUTE_OPERATOR_LOCK_PATH=""

gateway_execute_operator_lock_key() {
  local edge_name="${1:?edge chain name required}"
  local signer_config_path="${2:-}"
  local wallet_path gateway_config cast_bin

  [[ "${edge_name}" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]] || {
    echo "gateway-launch: invalid edge chain name: ${edge_name}" >&2
    return 1
  }
  wallet_path="${GATEWAY_DIR}/chains/${edge_name}/configs/wallets.yaml"
  gateway_config="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/ZkStack.yaml"
  cast_bin="$(command -v cast || true)"
  if [ -z "${cast_bin}" ] && [ -x "${HOME}/.foundry/bin/cast" ]; then
    cast_bin="${HOME}/.foundry/bin/cast"
  fi
  [ -n "${cast_bin}" ] || {
    echo "gateway-launch: cast is required to authenticate the execute_operator key" >&2
    return 1
  }

  python3 - "${wallet_path}" "${gateway_config}" "${signer_config_path}" "${cast_bin}" "$(dirname "${BASH_SOURCE[0]}")" <<'PY'
import sys
from pathlib import Path

import yaml

wallet_path = Path(sys.argv[1])
gateway_config = Path(sys.argv[2])
signer_config_path = Path(sys.argv[3]) if sys.argv[3] else None
cast_bin = sys.argv[4]
sys.path.insert(0, sys.argv[5])
from _wallet_identity import address_for_private_key, normalize_address

if not wallet_path.is_file():
    raise SystemExit(f"missing edge wallet file: {wallet_path}")
if not gateway_config.is_file():
    raise SystemExit(f"missing Gateway chain config: {gateway_config}")

wallets = yaml.safe_load(wallet_path.read_text(encoding="utf-8"))
wallet = wallets.get("execute_operator") if isinstance(wallets, dict) else None
address = normalize_address(
    wallet.get("address") if isinstance(wallet, dict) else None,
    f"execute_operator.address in {wallet_path}",
)
derived_address = address_for_private_key(
    wallet.get("private_key") if isinstance(wallet, dict) else None,
    f"execute_operator.private_key in {wallet_path}",
    cast_bin,
)
if derived_address != address:
    raise SystemExit(
        f"execute_operator address/private-key mismatch in {wallet_path}: "
        f"configured={address} derived={derived_address}"
    )

if signer_config_path is not None:
    if not signer_config_path.is_file():
        raise SystemExit(f"missing generated signer config: {signer_config_path}")
    signer_config = yaml.safe_load(signer_config_path.read_text(encoding="utf-8"))
    for section_name in ("l1_sender", "gateway_sender"):
        section = (
            signer_config.get(section_name)
            if isinstance(signer_config, dict)
            else None
        )
        signer_address = address_for_private_key(
            section.get("operator_execute_sk") if isinstance(section, dict) else None,
            f"{section_name}.operator_execute_sk in {signer_config_path}",
            cast_bin,
        )
        if signer_address != address:
            raise SystemExit(
                f"generated execute-operator signer mismatch for {wallet_path}: "
                f"wallet={address} {section_name}={signer_address}"
            )

gateway = yaml.safe_load(gateway_config.read_text(encoding="utf-8"))
chain_id = gateway.get("chain_id") if isinstance(gateway, dict) else None
if isinstance(chain_id, str):
    raw_chain_id = chain_id.strip()
    try:
        chain_id = int(
            raw_chain_id,
            16 if raw_chain_id.lower().startswith("0x") else 10,
        )
    except ValueError:
        raise SystemExit(f"invalid chain_id in {gateway_config}") from None
if (
    not isinstance(chain_id, int)
    or isinstance(chain_id, bool)
    or chain_id <= 0
    or chain_id >= 2**256
):
    raise SystemExit(f"invalid chain_id in {gateway_config}")

# Same Gateway chain ID and EOA deliberately serialize even across edge names.
print(f"gateway-{chain_id}-execute-operator-{address[2:]}")
PY
}

gateway_execute_operator_lock_fd_matches_path() {
  local lock_path="${1:?lock path required}"

  python3 - "${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" "${lock_path}" <<'PY'
import fcntl
import os
import sys

fd = int(sys.argv[1])
path = sys.argv[2]
try:
    fd_stat = os.fstat(fd)
    path_stat = os.stat(path)
except OSError:
    raise SystemExit(1)
if (fd_stat.st_dev, fd_stat.st_ino) != (path_stat.st_dev, path_stat.st_ino):
    raise SystemExit(1)
try:
    # Re-locking an inherited copy of the same open file description is
    # idempotent; a distinct process/open description still fails closed.
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    raise SystemExit(1)
PY
}

gateway_acquire_execute_operator_lock() {
  local edge_name="${1:?edge chain name required}"
  local signer_config_path="${2:-}"
  local lock_key lock_root lock_path

  lock_key="$(gateway_execute_operator_lock_key "${edge_name}" "${signer_config_path}")" || return 1
  lock_root="${GATEWAY_DIR}/.gateway-launch-locks"
  lock_path="${lock_root}/${lock_key}.lock"

  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY}" ]; then
    if [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY}" != "${lock_key}" ] ||
      [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_PATH}" != "${lock_path}" ] ||
      ! gateway_execute_operator_lock_fd_matches_path "${lock_path}"; then
      echo "gateway-launch: a different or invalid execute_operator lock is already held" >&2
      return 1
    fi
    return 0
  fi

  mkdir -p "${lock_root}"
  chmod 700 "${lock_root}"
  : >"${lock_path}"
  chmod 600 "${lock_path}"

  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD:-}" ]; then
    if [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD}" != "${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" ] ||
      ! gateway_execute_operator_lock_fd_matches_path "${lock_path}"; then
      echo "gateway-launch: invalid inherited execute_operator lock for ${edge_name}" >&2
      return 1
    fi
  else
    exec 9>"${lock_path}"
    if ! python3 - "${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" <<'PY'
import fcntl
import sys

try:
    fcntl.flock(int(sys.argv[1]), fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    raise SystemExit(1)
PY
    then
      exec 9>&-
      echo "gateway-launch: ${edge_name} execute_operator is in use; stop its edge node or other provisioning process before retrying" >&2
      return 1
    fi
  fi

  GATEWAY_EXECUTE_OPERATOR_LOCK_KEY="${lock_key}"
  GATEWAY_EXECUTE_OPERATOR_LOCK_PATH="${lock_path}"
}

gateway_release_execute_operator_lock() {
  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY:-}" ]; then
    exec 9>&- || true
  fi
  GATEWAY_EXECUTE_OPERATOR_LOCK_KEY=""
  GATEWAY_EXECUTE_OPERATOR_LOCK_PATH=""
}
