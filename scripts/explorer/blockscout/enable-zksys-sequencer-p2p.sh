#!/usr/bin/env bash
set -euo pipefail

SEQUENCER_REMOTE_HOST="${SEQUENCER_REMOTE_HOST:-}"
SSH_KEY_PATH="${SSH_KEY_PATH:-}"

ZKSYS_CONFIG_PATH="${ZKSYS_CONFIG_PATH:-/home/ubuntu/gateway/os-server-configs/zksys/config.yaml}"
ZKSYS_NETWORK_SECRET_PATH="${ZKSYS_NETWORK_SECRET_PATH:-/home/ubuntu/gateway/os-server-configs/zksys/network.secret}"
ZKSYS_P2P_HOST="${ZKSYS_P2P_HOST:-}"
ZKSYS_P2P_ADDRESS="${ZKSYS_P2P_ADDRESS:-}"
ZKSYS_P2P_PORT="${ZKSYS_P2P_PORT:-3060}"
RESTART_ZKSYS="${RESTART_ZKSYS:-0}"
ZKSYS_SERVICE_NAME="${ZKSYS_SERVICE_NAME:-}"

if [[ -z "${SEQUENCER_REMOTE_HOST}" ]]; then
  echo "SEQUENCER_REMOTE_HOST is required, for example ubuntu@148.251.44.149" >&2
  exit 1
fi

if [[ -z "${ZKSYS_P2P_HOST}" ]]; then
  ZKSYS_P2P_HOST="${SEQUENCER_REMOTE_HOST#*@}"
fi
if [[ -z "${ZKSYS_P2P_ADDRESS}" ]]; then
  ZKSYS_P2P_ADDRESS="$(
    python3 - "${ZKSYS_P2P_HOST}" <<'PY'
import ipaddress
import socket
import sys

host = sys.argv[1]
try:
    print(ipaddress.IPv4Address(host))
    raise SystemExit(0)
except ipaddress.AddressValueError:
    pass

for family, _, _, _, sockaddr in socket.getaddrinfo(host, None, socket.AF_INET, socket.SOCK_STREAM):
    if family == socket.AF_INET:
        print(sockaddr[0])
        raise SystemExit(0)

raise SystemExit(f"could not resolve {host!r} to an IPv4 address; set ZKSYS_P2P_ADDRESS explicitly")
PY
  )"
fi

ssh_opts=(-o StrictHostKeyChecking=accept-new)
if [[ -n "${SSH_KEY_PATH}" ]]; then
  ssh_opts+=(-i "${SSH_KEY_PATH}")
fi

ssh "${ssh_opts[@]}" "${SEQUENCER_REMOTE_HOST}" bash -s -- \
  "${ZKSYS_CONFIG_PATH}" \
  "${ZKSYS_NETWORK_SECRET_PATH}" \
  "${ZKSYS_P2P_HOST}" \
  "${ZKSYS_P2P_ADDRESS}" \
  "${ZKSYS_P2P_PORT}" \
  "${RESTART_ZKSYS}" \
  "${ZKSYS_SERVICE_NAME:-__EMPTY__}" <<'REMOTE_SCRIPT'
set -euo pipefail

ZKSYS_CONFIG_PATH="$1"
ZKSYS_NETWORK_SECRET_PATH="$2"
ZKSYS_P2P_HOST="$3"
ZKSYS_P2P_ADDRESS="$4"
ZKSYS_P2P_PORT="$5"
RESTART_ZKSYS="$6"
ZKSYS_SERVICE_NAME="${7:-__EMPTY__}"
if [[ "${ZKSYS_SERVICE_NAME}" == "__EMPTY__" ]]; then
  ZKSYS_SERVICE_NAME=""
fi

if [[ ! -f "${ZKSYS_CONFIG_PATH}" ]]; then
  echo "missing zksys config: ${ZKSYS_CONFIG_PATH}" >&2
  exit 1
fi

command -v python3 >/dev/null || {
  echo "python3 is required on the sequencer host" >&2
  exit 1
}
command -v openssl >/dev/null || {
  echo "openssl is required on the sequencer host" >&2
  exit 1
}

umask 077
python3 - \
  "${ZKSYS_CONFIG_PATH}" \
  "${ZKSYS_NETWORK_SECRET_PATH}" \
  "${ZKSYS_P2P_HOST}" \
  "${ZKSYS_P2P_ADDRESS}" \
  "${ZKSYS_P2P_PORT}" <<'PY'
import json
import os
import secrets
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml


def normalize_key(raw: str) -> str:
    key = raw.strip()
    if key.startswith("0x"):
        key = key[2:]
    if len(key) != 64:
        raise SystemExit("network secret must be 32 bytes hex")
    int(key, 16)
    if int(key, 16) == 0:
        raise SystemExit("network secret must be non-zero")
    return "0x" + key.lower()


def read_or_create_key(path: Path) -> str:
    if path.exists():
        existing = path.read_text(encoding="utf-8").strip()
        if existing:
            return normalize_key(existing)
    key = "0x" + secrets.token_hex(32)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp.write_text(key + "\n", encoding="utf-8")
    tmp.chmod(0o600)
    tmp.replace(path)
    path.chmod(0o600)
    return key


def sec1_der_without_public_key(secret_hex: str) -> bytes:
    secret = bytes.fromhex(secret_hex.removeprefix("0x"))
    # RFC 5915 ECPrivateKey for secp256k1, without the optional publicKey field:
    # SEQUENCE(version=1, privateKey=32 bytes, parameters=OID secp256k1)
    return bytes.fromhex("302e0201010420") + secret + bytes.fromhex("a00706052b8104000a")


def derive_enode_id(secret_key: str) -> str:
    with tempfile.TemporaryDirectory() as tmpdir:
        der_path = Path(tmpdir) / "key.der"
        der_path.write_bytes(sec1_der_without_public_key(secret_key))
        pub_pem = subprocess.check_output(
            ["openssl", "ec", "-inform", "DER", "-in", str(der_path), "-pubout"],
            stderr=subprocess.DEVNULL,
        )
        pub_text = subprocess.check_output(
            ["openssl", "ec", "-pubin", "-pubout", "-outform", "DER"],
            input=pub_pem,
            stderr=subprocess.DEVNULL,
        )
    # DER SubjectPublicKeyInfo for secp256k1 ends with BIT STRING 0x03 0x42 0x00
    # followed by the 65-byte uncompressed pubkey. devp2p enode id drops the 0x04 prefix.
    uncompressed = pub_text[-65:]
    if len(uncompressed) != 65 or uncompressed[0] != 4:
        raise SystemExit("failed to derive uncompressed secp256k1 public key")
    return uncompressed[1:].hex()


config_path = Path(sys.argv[1])
secret_path = Path(sys.argv[2])
p2p_host = sys.argv[3]
p2p_address = sys.argv[4]
p2p_port = int(sys.argv[5])

secret_key = read_or_create_key(secret_path)
data = yaml.safe_load(config_path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML config: {config_path}")

network = data.setdefault("network", {})
if not isinstance(network, dict):
    raise SystemExit("config.network must be a mapping if present")
network["enabled"] = True
network["secret_key"] = secret_key
network["address"] = p2p_address
network["port"] = p2p_port

tmp = config_path.with_name(f".{config_path.name}.{os.getpid()}.tmp")
tmp.write_text(yaml.safe_dump(data, sort_keys=False), encoding="utf-8")
tmp.chmod(0o600)
tmp.replace(config_path)
config_path.chmod(0o600)

enode_id = derive_enode_id(secret_key)
enode = f"enode://{enode_id}@{p2p_host}:{p2p_port}"
print(f"MAIN_NODE_ENODE={json.dumps(enode)}")
print(f"ZKSYS_P2P_CONFIG={config_path}")
print(f"ZKSYS_NETWORK_SECRET={secret_path}")
PY

if [[ "${RESTART_ZKSYS}" == "1" ]]; then
  if [[ -n "${ZKSYS_SERVICE_NAME}" ]]; then
    sudo systemctl restart "${ZKSYS_SERVICE_NAME}"
  else
    pkill -TERM -f "zksync-os-server --config ${ZKSYS_CONFIG_PATH}" || true
    sleep 2
    nohup /home/ubuntu/gateway/os-server-configs/zksys/start-node.sh \
      > /home/ubuntu/zksys-node.log 2>&1 &
    echo $! > /home/ubuntu/zksys-node.pid
  fi
fi
REMOTE_SCRIPT
