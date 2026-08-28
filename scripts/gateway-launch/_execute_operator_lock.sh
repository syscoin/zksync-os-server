#!/usr/bin/env bash
# SYSCOIN: Shared Gateway execute-operator nonce lock. This file is meant to be sourced.

if [ "${SYSCOIN_EXECUTE_OPERATOR_LOCK_LIBRARY_LOADED:-false}" = "true" ]; then
  return 0
fi
SYSCOIN_EXECUTE_OPERATOR_LOCK_LIBRARY_LOADED=true

readonly GATEWAY_EXECUTE_OPERATOR_LOCK_FD=9
GATEWAY_EXECUTE_OPERATOR_LOCK_KEY=""
GATEWAY_EXECUTE_OPERATOR_LOCK_PATH=""
GATEWAY_EXECUTE_OPERATOR_LOCK_ROOT_ID=""
GATEWAY_EXECUTE_OPERATOR_LOCK_ID=""

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

gateway_execute_operator_lock_prepare() {
  local lock_root="${1:?lock root required}"
  local lock_name="${2:?lock name required}"

  python3 - "${lock_root}" "${lock_name}" <<'PY'
import os
import stat
import sys

root_path = sys.argv[1]
lock_name = sys.argv[2]
if not lock_name or os.path.basename(lock_name) != lock_name:
    raise SystemExit("invalid execute_operator lock name")

def validate(info: os.stat_result, label: str) -> None:
    if not stat.S_ISDIR(info.st_mode):
        raise SystemExit(f"execute_operator {label} must be a non-symlink directory")
    if info.st_uid != os.geteuid():
        raise SystemExit(f"execute_operator {label} must be owned by the launching user")
    if stat.S_IMODE(info.st_mode) != 0o700:
        raise SystemExit(f"execute_operator {label} must have mode 0700")

old_umask = os.umask(0o077)
root_fd = lock_fd = None
try:
    try:
        os.mkdir(root_path, 0o700)
    except FileExistsError:
        pass
    root_path_info = os.lstat(root_path)
    validate(root_path_info, f"lock root {root_path}")
    root_fd = os.open(
        root_path,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
    )
    root_fd_info = os.fstat(root_fd)
    root_path_info = os.lstat(root_path)
    validate(root_fd_info, f"lock root {root_path}")
    validate(root_path_info, f"lock root {root_path}")
    if (root_path_info.st_dev, root_path_info.st_ino) != (
        root_fd_info.st_dev,
        root_fd_info.st_ino,
    ):
        raise SystemExit(f"execute_operator lock root identity changed: {root_path}")

    try:
        os.mkdir(lock_name, 0o700, dir_fd=root_fd)
    except FileExistsError:
        pass
    lock_path_info = os.stat(lock_name, dir_fd=root_fd, follow_symlinks=False)
    validate(lock_path_info, f"lock {lock_name}")
    lock_fd = os.open(
        lock_name,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=root_fd,
    )
    lock_fd_info = os.fstat(lock_fd)
    lock_path_info = os.stat(lock_name, dir_fd=root_fd, follow_symlinks=False)
    root_path_info = os.lstat(root_path)
    validate(lock_fd_info, f"lock {lock_name}")
    validate(lock_path_info, f"lock {lock_name}")
    validate(root_path_info, f"lock root {root_path}")
    if (root_path_info.st_dev, root_path_info.st_ino) != (
        root_fd_info.st_dev,
        root_fd_info.st_ino,
    ):
        raise SystemExit(f"execute_operator lock root identity changed: {root_path}")
    if (lock_path_info.st_dev, lock_path_info.st_ino) != (
        lock_fd_info.st_dev,
        lock_fd_info.st_ino,
    ):
        raise SystemExit(f"execute_operator lock identity changed: {lock_name}")
    print(
        f"{root_fd_info.st_dev}:{root_fd_info.st_ino} "
        f"{lock_fd_info.st_dev}:{lock_fd_info.st_ino}"
    )
finally:
    if lock_fd is not None:
        os.close(lock_fd)
    if root_fd is not None:
        os.close(root_fd)
    os.umask(old_umask)
PY
}

gateway_execute_operator_lock_fd_matches_path() {
  local lock_path="${1:?lock path required}"
  local expected_root_id="${2:?lock root identity required}"
  local expected_lock_id="${3:?lock identity required}"

  python3 - \
    "${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" \
    "${lock_path}" \
    "${expected_root_id}" \
    "${expected_lock_id}" <<'PY'
import fcntl
import os
import stat
import sys

lock_fd = int(sys.argv[1])
lock_path = sys.argv[2]
expected_root_id = sys.argv[3]
expected_lock_id = sys.argv[4]
root_path, lock_name = os.path.split(lock_path)
if not root_path or not lock_name:
    raise SystemExit(1)

def validate_info(info: os.stat_result) -> None:
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.geteuid()
        or stat.S_IMODE(info.st_mode) != 0o700
    ):
        raise SystemExit(1)

def validate_snapshot() -> None:
    root_fd = None
    try:
        root_fd = os.open(
            root_path,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        )
        root_fd_info = os.fstat(root_fd)
        root_path_info = os.lstat(root_path)
        lock_path_info = os.stat(
            lock_name,
            dir_fd=root_fd,
            follow_symlinks=False,
        )
        lock_fd_info = os.fstat(lock_fd)
        for info in (root_fd_info, root_path_info, lock_path_info, lock_fd_info):
            validate_info(info)
        if (root_path_info.st_dev, root_path_info.st_ino) != (
            root_fd_info.st_dev,
            root_fd_info.st_ino,
        ):
            raise SystemExit(1)
        if f"{root_fd_info.st_dev}:{root_fd_info.st_ino}" != expected_root_id:
            raise SystemExit(1)
        if (lock_path_info.st_dev, lock_path_info.st_ino) != (
            lock_fd_info.st_dev,
            lock_fd_info.st_ino,
        ):
            raise SystemExit(1)
        if f"{lock_fd_info.st_dev}:{lock_fd_info.st_ino}" != expected_lock_id:
            raise SystemExit(1)
    except OSError:
        raise SystemExit(1) from None
    finally:
        if root_fd is not None:
            os.close(root_fd)

validate_snapshot()
try:
    # Re-locking an inherited copy of the same open file description is
    # idempotent; a distinct process/open description still fails closed.
    fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except (BlockingIOError, OSError):
    raise SystemExit(1) from None
validate_snapshot()
PY
}

gateway_acquire_execute_operator_lock() {
  local edge_name="${1:?edge chain name required}"
  local signer_config_path="${2:-}"
  local identities lock_id lock_key lock_name lock_path lock_root root_id extra

  lock_key="$(gateway_execute_operator_lock_key "${edge_name}" "${signer_config_path}")" || return 1
  lock_root="${GATEWAY_DIR}/.gateway-launch-locks"
  lock_name="${lock_key}.lock"
  lock_path="${lock_root}/${lock_name}"

  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY}" ]; then
    if [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY}" != "${lock_key}" ] ||
      [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_PATH}" != "${lock_path}" ] ||
      ! gateway_execute_operator_lock_fd_matches_path \
        "${lock_path}" \
        "${GATEWAY_EXECUTE_OPERATOR_LOCK_ROOT_ID}" \
        "${GATEWAY_EXECUTE_OPERATOR_LOCK_ID}"; then
      echo "gateway-launch: a different or invalid execute_operator lock is already held" >&2
      return 1
    fi
    return 0
  fi

  # SYSCOIN: Within the trusted GATEWAY_DIR namespace, prepare only
  # owner-private directories. Opening the held FD read-only through `/.`
  # cannot redirect a mutation, and every inode mismatch fails validation.
  identities="$(gateway_execute_operator_lock_prepare "${lock_root}" "${lock_name}")" || return 1
  read -r root_id lock_id extra <<<"${identities}"
  if [ -z "${root_id}" ] || [ -z "${lock_id}" ] || [ -n "${extra}" ]; then
    echo "gateway-launch: invalid execute_operator lock identities for ${edge_name}" >&2
    return 1
  fi

  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD:-}" ]; then
    if [ "${GATEWAY_EXECUTE_OPERATOR_LOCK_INHERIT_FD}" != "${GATEWAY_EXECUTE_OPERATOR_LOCK_FD}" ] ||
      ! gateway_execute_operator_lock_fd_matches_path \
        "${lock_path}" "${root_id}" "${lock_id}"; then
      echo "gateway-launch: invalid inherited execute_operator lock for ${edge_name}" >&2
      return 1
    fi
  else
    if ! exec 9<"${lock_path}/."; then
      echo "gateway-launch: could not open execute_operator lock for ${edge_name}" >&2
      return 1
    fi
    if ! gateway_execute_operator_lock_fd_matches_path \
      "${lock_path}" "${root_id}" "${lock_id}"; then
      exec 9>&-
      echo "gateway-launch: ${edge_name} execute_operator is in use; stop its edge node or other provisioning process before retrying" >&2
      return 1
    fi
  fi

  GATEWAY_EXECUTE_OPERATOR_LOCK_KEY="${lock_key}"
  GATEWAY_EXECUTE_OPERATOR_LOCK_PATH="${lock_path}"
  GATEWAY_EXECUTE_OPERATOR_LOCK_ROOT_ID="${root_id}"
  GATEWAY_EXECUTE_OPERATOR_LOCK_ID="${lock_id}"
}

gateway_release_execute_operator_lock() {
  if [ -n "${GATEWAY_EXECUTE_OPERATOR_LOCK_KEY:-}" ]; then
    exec 9>&- || true
  fi
  GATEWAY_EXECUTE_OPERATOR_LOCK_KEY=""
  GATEWAY_EXECUTE_OPERATOR_LOCK_PATH=""
  GATEWAY_EXECUTE_OPERATOR_LOCK_ROOT_ID=""
  GATEWAY_EXECUTE_OPERATOR_LOCK_ID=""
}
