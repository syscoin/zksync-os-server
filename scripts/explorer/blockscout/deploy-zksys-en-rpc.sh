#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

REMOTE_HOST="${REMOTE_HOST:-}"
SEQUENCER_REMOTE_HOST="${SEQUENCER_REMOTE_HOST:-}"
SSH_KEY_PATH="${SSH_KEY_PATH:-}"

REMOTE_BASE_DIR="${REMOTE_BASE_DIR:-/home/ubuntu/zksync-en}"
REMOTE_OS_SERVER_PATH="${REMOTE_OS_SERVER_PATH:-/home/ubuntu/zksync-os-server}"
UPLOAD_REPO="${UPLOAD_REPO:-true}"
START_SERVICES="${START_SERVICES:-true}"
START_PUBLIC_SERVICE="${START_PUBLIC_SERVICE:-${START_SERVICES}}"
START_DEBUG_SERVICE="${START_DEBUG_SERVICE:-${START_SERVICES}}"
INSTALL_BUILD_DEPS="${INSTALL_BUILD_DEPS:-true}"
DEFAULT_GATEWAY_RPC_URL="${DEFAULT_GATEWAY_RPC_URL:-https://rpc-gw.tanenbaum.io}"

SOURCE_ZKSYS_CONFIG="${SOURCE_ZKSYS_CONFIG:-/home/ubuntu/gateway/os-server-configs/zksys/config.yaml}"
SOURCE_GATEWAY_DIR="${SOURCE_GATEWAY_DIR:-/home/ubuntu/gateway}"
GATEWAY_CHAIN_NAME="${GATEWAY_CHAIN_NAME:-gateway}"
SYSCOIN_EDGE_DA_COMMIT_TARGET="${SYSCOIN_EDGE_DA_COMMIT_TARGET:-${ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET:-}}"
MAIN_NODE_ENODE="${MAIN_NODE_ENODE:-}"
MAIN_NODE_RPC_URL="${MAIN_NODE_RPC_URL:-}"
MAIN_NODE_RPC_PORT="${MAIN_NODE_RPC_PORT:-3050}"
CHAIN_ID="${CHAIN_ID:-57057}"
PROTOCOL_VERSION="${PROTOCOL_VERSION:-v31.0}"

PUBLIC_RPC_BIND_HOST="${PUBLIC_RPC_BIND_HOST:-127.0.0.1}"
PUBLIC_RPC_PORT="${PUBLIC_RPC_PORT:-3050}"
PUBLIC_P2P_ADDRESS="${PUBLIC_P2P_ADDRESS:-0.0.0.0}"
PUBLIC_P2P_PORT="${PUBLIC_P2P_PORT:-3061}"
PUBLIC_STATUS_PORT="${PUBLIC_STATUS_PORT:-3073}"
PUBLIC_PROMETHEUS_PORT="${PUBLIC_PROMETHEUS_PORT:-3314}"

# Blockscout containers reach the host through `host.docker.internal`. On Linux this maps to the
# Docker host gateway, so the debug RPC must bind to a host address reachable from Docker, not
# only to 127.0.0.1. If unset, the remote installer resolves docker0 and fails closed if it
# cannot determine a Docker-reachable bind address.
DEBUG_RPC_BIND_HOST="${DEBUG_RPC_BIND_HOST:-}"
DEBUG_RPC_PORT="${DEBUG_RPC_PORT:-3051}"
DEBUG_P2P_ADDRESS="${DEBUG_P2P_ADDRESS:-0.0.0.0}"
DEBUG_P2P_PORT="${DEBUG_P2P_PORT:-3062}"
DEBUG_STATUS_PORT="${DEBUG_STATUS_PORT:-3074}"
DEBUG_PROMETHEUS_PORT="${DEBUG_PROMETHEUS_PORT:-3315}"
# Per-method RPC rate limits, "method=requests_per_second" comma-separated; empty disables
# rate limiting entirely. The debug EN binds to the docker0 address and only serves the local
# Blockscout indexer, so it runs unlimited by default — throttling it just makes the internal
# transactions backfill fail with -32005 and retry. The public EN is the place to add limits
# (its debug namespace is disabled, so only the cheap eth_* surface is exposed).
DEBUG_RPC_RATE_LIMITS="${DEBUG_RPC_RATE_LIMITS:-}"
PUBLIC_RPC_RATE_LIMITS="${PUBLIC_RPC_RATE_LIMITS:-}"
DEBUG_RPC_BIND_HOST_ARG="${DEBUG_RPC_BIND_HOST:-__AUTO__}"

if [[ -z "${REMOTE_HOST}" ]]; then
  echo "REMOTE_HOST is required, for example ubuntu@explorer-host" >&2
  exit 1
fi

if [[ -z "${MAIN_NODE_ENODE}" ]]; then
  cat >&2 <<'EOF'
MAIN_NODE_ENODE is required.
Enable p2p on the zksys sequencer first, then pass its enode, e.g.
MAIN_NODE_ENODE='enode://<main-node-peer-id>@148.251.44.149:3060'
EOF
  exit 1
fi

if [[ -z "${MAIN_NODE_RPC_URL}" ]]; then
  if [[ -z "${SEQUENCER_REMOTE_HOST}" ]]; then
    cat >&2 <<'EOF'
MAIN_NODE_RPC_URL or SEQUENCER_REMOTE_HOST is required.
After the public RPC DNS points at the EN, this must be a direct sequencer RPC
URL reachable from the EN host, usually allowlisted to the explorer host only.
EOF
    exit 1
  fi
  MAIN_NODE_RPC_URL="http://${SEQUENCER_REMOTE_HOST#*@}:${MAIN_NODE_RPC_PORT}"
fi

normalize_syscoin_edge_da_commit_target() {
  TARGET="$1" python3 - <<'PY'
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

shell_join() {
  # SYSCOIN: quote env-derived arguments before embedding them in SSH remote
  # command strings; OpenSSH remote commands are still parsed by a shell.
  python3 - "$@" <<'PY'
import shlex
import sys

print(" ".join(shlex.quote(arg) for arg in sys.argv[1:]))
PY
}

ssh_opts=(-o StrictHostKeyChecking=accept-new)
if [[ -n "${SSH_KEY_PATH}" ]]; then
  ssh_opts+=(-i "${SSH_KEY_PATH}")
fi

provider_json=""
if [[ -n "${ZKSYS_EN_L1_RPC_URL:-}" ]]; then
  provider_json="$(
    python3 - <<'PY'
import json
import os

provider = {
    "l1_provider": {"rpc_url": os.environ["ZKSYS_EN_L1_RPC_URL"]},
    "gateway_provider": {"rpc_url": os.environ.get("ZKSYS_EN_GATEWAY_RPC_URL", "https://rpc-gw.tanenbaum.io/")},
}
print(json.dumps(provider, separators=(",", ":")))
PY
  )"
elif [[ -n "${SEQUENCER_REMOTE_HOST}" ]]; then
  remote_provider_cmd="python3 - $(shell_join "${SOURCE_ZKSYS_CONFIG}" "${DEFAULT_GATEWAY_RPC_URL}")"
  provider_json="$(
    ssh "${ssh_opts[@]}" "${SEQUENCER_REMOTE_HOST}" \
      "${remote_provider_cmd}" <<'PY'
import json
import sys
from urllib.parse import urlparse
from pathlib import Path

import yaml

config_path = Path(sys.argv[1])
default_gateway_rpc_url = sys.argv[2]
data = yaml.safe_load(config_path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid zksys config: {config_path}")

providers = {
    "l1_provider": data.get("l1_provider") or {},
    "gateway_provider": data.get("gateway_provider") or {},
}
if not providers["l1_provider"].get("rpc_url"):
    raise SystemExit(f"missing l1_provider.rpc_url in {config_path}")
if not providers["gateway_provider"].get("rpc_url"):
    raise SystemExit(f"missing gateway_provider.rpc_url in {config_path}")

gateway_rpc_url = providers["gateway_provider"]["rpc_url"]
gateway_rpc_host = urlparse(gateway_rpc_url).hostname
if gateway_rpc_host in {"127.0.0.1", "localhost", "::1"}:
    providers["gateway_provider"]["rpc_url"] = default_gateway_rpc_url

l1_rpc_url = providers["l1_provider"]["rpc_url"]
l1_rpc_host = urlparse(l1_rpc_url).hostname
if l1_rpc_host in {"127.0.0.1", "localhost", "::1"}:
    raise SystemExit(
        "l1_provider.rpc_url is loopback in the sequencer config; "
        "set ZKSYS_EN_L1_RPC_URL to an L1 RPC URL reachable from the EN host"
    )
print(json.dumps(providers, separators=(",", ":")))
PY
  )"
else
  cat >&2 <<'EOF'
Set SEQUENCER_REMOTE_HOST so this script can copy provider RPC URLs from the sequencer config,
or set ZKSYS_EN_L1_RPC_URL and ZKSYS_EN_GATEWAY_RPC_URL explicitly.
EOF
  exit 1
fi

if [[ -z "${SYSCOIN_EDGE_DA_COMMIT_TARGET}" ]]; then
  local_gateway_config="${SOURCE_GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/gateway.yaml"
  if [[ -f "${local_gateway_config}" ]]; then
    SYSCOIN_EDGE_DA_COMMIT_TARGET="$(
      python3 - "${local_gateway_config}" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid Gateway config: {path}")
addr = data.get("validator_timelock_addr")
if isinstance(addr, int):
    addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
if not isinstance(addr, str) or not addr.strip():
    raise SystemExit(f"missing validator_timelock_addr in {path}")
print(addr.strip())
PY
    )"
  elif [[ -z "${SEQUENCER_REMOTE_HOST}" ]]; then
    cat >&2 <<'EOF'
SYSCOIN_EDGE_DA_COMMIT_TARGET is required when neither SOURCE_GATEWAY_DIR nor SEQUENCER_REMOTE_HOST can provide it.
Set it to the Gateway validator_timelock_addr used by the patched zksync-os binary.
EOF
    exit 1
  else
    remote_gateway_config="${SOURCE_GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/gateway.yaml"
    remote_gateway_config_cmd="python3 - $(shell_join "${remote_gateway_config}")"
    SYSCOIN_EDGE_DA_COMMIT_TARGET="$(
      ssh "${ssh_opts[@]}" "${SEQUENCER_REMOTE_HOST}" \
        "${remote_gateway_config_cmd}" <<'PY'
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid Gateway config: {path}")
addr = data.get("validator_timelock_addr")
if isinstance(addr, int):
    addr = "0x" + format(addr & ((1 << 160) - 1), "040x")
if not isinstance(addr, str) or not addr.strip():
    raise SystemExit(f"missing validator_timelock_addr in {path}")
print(addr.strip())
PY
    )"
  fi
fi
SYSCOIN_EDGE_DA_COMMIT_TARGET="$(normalize_syscoin_edge_da_commit_target "${SYSCOIN_EDGE_DA_COMMIT_TARGET}")"

provider_b64="$(printf '%s' "${provider_json}" | base64 | tr -d '\n')"

if [[ "${UPLOAD_REPO}" == "true" ]]; then
  echo "Uploading zksync-os-server tree to ${REMOTE_HOST}:${REMOTE_OS_SERVER_PATH}"
  remote_os_server_path_q="$(shell_join "${REMOTE_OS_SERVER_PATH}")"
  tar \
    --exclude .git \
    --exclude target \
    --exclude .cursor \
    --exclude build-artifacts \
    -C "${REPO_ROOT}" \
    -cf - . | ssh "${ssh_opts[@]}" "${REMOTE_HOST}" \
      "set -euo pipefail; mkdir -p ${remote_os_server_path_q}; tar -C ${remote_os_server_path_q} -xf -"
fi

remote_install_cmd="bash -s -- $(shell_join \
  "${provider_b64}" \
  "${REMOTE_BASE_DIR}" \
  "${REMOTE_OS_SERVER_PATH}" \
  "${MAIN_NODE_ENODE}" \
  "${MAIN_NODE_RPC_URL}" \
  "${CHAIN_ID}" \
  "${PROTOCOL_VERSION}" \
  "${PUBLIC_RPC_BIND_HOST}" \
  "${PUBLIC_RPC_PORT}" \
  "${PUBLIC_P2P_ADDRESS}" \
  "${PUBLIC_P2P_PORT}" \
  "${PUBLIC_STATUS_PORT}" \
  "${PUBLIC_PROMETHEUS_PORT}" \
  "${DEBUG_RPC_BIND_HOST_ARG}" \
  "${DEBUG_RPC_PORT}" \
  "${DEBUG_P2P_ADDRESS}" \
  "${DEBUG_P2P_PORT}" \
  "${DEBUG_STATUS_PORT}" \
  "${DEBUG_PROMETHEUS_PORT}" \
  "${DEBUG_RPC_RATE_LIMITS}" \
  "${START_SERVICES}" \
  "${INSTALL_BUILD_DEPS}" \
  "${START_PUBLIC_SERVICE}" \
  "${START_DEBUG_SERVICE}" \
  "${PUBLIC_RPC_RATE_LIMITS}" \
  "${SYSCOIN_EDGE_DA_COMMIT_TARGET}")"

ssh "${ssh_opts[@]}" "${REMOTE_HOST}" "${remote_install_cmd}" <<'REMOTE_SCRIPT'
set -euo pipefail

PROVIDER_B64="$1"
REMOTE_BASE_DIR="$2"
REMOTE_OS_SERVER_PATH="$3"
MAIN_NODE_ENODE="$4"
MAIN_NODE_RPC_URL="$5"
CHAIN_ID="$6"
PROTOCOL_VERSION="$7"
PUBLIC_RPC_BIND_HOST="$8"
PUBLIC_RPC_PORT="$9"
PUBLIC_P2P_ADDRESS="${10}"
PUBLIC_P2P_PORT="${11}"
PUBLIC_STATUS_PORT="${12}"
PUBLIC_PROMETHEUS_PORT="${13}"
DEBUG_RPC_BIND_HOST="${14}"
DEBUG_RPC_PORT="${15}"
DEBUG_P2P_ADDRESS="${16}"
DEBUG_P2P_PORT="${17}"
DEBUG_STATUS_PORT="${18}"
DEBUG_PROMETHEUS_PORT="${19}"
DEBUG_RPC_RATE_LIMITS="${20}"
START_SERVICES="${21}"
INSTALL_BUILD_DEPS="${22}"
START_PUBLIC_SERVICE="${23}"
START_DEBUG_SERVICE="${24}"
PUBLIC_RPC_RATE_LIMITS="${25}"
SYSCOIN_EDGE_DA_COMMIT_TARGET="${26}"

if [[ ! -d "${REMOTE_OS_SERVER_PATH}" ]]; then
  echo "missing remote zksync-os-server checkout: ${REMOTE_OS_SERVER_PATH}" >&2
  exit 1
fi

command -v python3 >/dev/null || {
  echo "python3 is required on the explorer host" >&2
  exit 1
}
command -v openssl >/dev/null || {
  echo "openssl is required on the explorer host" >&2
  exit 1
}

if [[ "${INSTALL_BUILD_DEPS}" == "true" ]]; then
  apt_packages=()
  for package in \
    build-essential \
    pkg-config \
    libssl-dev \
    clang \
    lld \
    cmake \
    protobuf-compiler \
    libclang-dev \
    git \
    curl \
    jq \
    unzip \
    zip \
    ca-certificates \
    python3 \
    python3-pip \
    python3-venv \
    tmux \
    screen \
    expect \
    moreutils \
    gnupg; do
    dpkg-query -W -f='${Status}' "${package}" 2>/dev/null | grep -q "install ok installed" || apt_packages+=("${package}")
  done

  if [[ "${#apt_packages[@]}" -gt 0 ]]; then
    sudo apt-get update
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y "${apt_packages[@]}"
  fi

  if [[ ! -x "${HOME}/.cargo/bin/rustup" ]]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  fi
  export PATH="${HOME}/.cargo/bin:${PATH}"
  rust_toolchain="$(
    python3 - "${REMOTE_OS_SERVER_PATH}/rust-toolchain.toml" <<'PY'
import re
import sys
from pathlib import Path

toolchain_toml = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'(?m)^\s*channel\s*=\s*["\']([^"\']+)["\']\s*$', toolchain_toml)
if match is None:
    raise SystemExit(f"missing toolchain.channel in {sys.argv[1]}")
print(match.group(1))
PY
  )"
  rustup toolchain install "${rust_toolchain}" --profile minimal
  rustup target add riscv32i-unknown-none-elf --toolchain "${rust_toolchain}"
  rustup component add llvm-tools-preview rust-src --toolchain "${rust_toolchain}"
  cargo install cargo-binutils --locked || true
fi

if [[ "${DEBUG_RPC_BIND_HOST}" == "__AUTO__" || -z "${DEBUG_RPC_BIND_HOST}" ]]; then
  DEBUG_RPC_BIND_HOST="$(ip -4 addr show docker0 2>/dev/null | awk '/inet / { sub(/\/.*/, "", $2); print $2; exit }' || true)"
  if [[ -z "${DEBUG_RPC_BIND_HOST}" ]]; then
    cat >&2 <<'EOF'
Could not auto-detect a Docker-reachable debug RPC bind address from docker0.
Set DEBUG_RPC_BIND_HOST explicitly to the host address that Blockscout containers
can reach through host.docker.internal. Do not use 0.0.0.0 unless the host firewall
or cloud security group restricts DEBUG_RPC_PORT to trusted clients.
EOF
    exit 1
  fi
fi

install -d -m 0755 "${REMOTE_BASE_DIR}"

python3 - \
  "${PROVIDER_B64}" \
  "${REMOTE_BASE_DIR}" \
  "${REMOTE_OS_SERVER_PATH}" \
  "${MAIN_NODE_ENODE}" \
  "${MAIN_NODE_RPC_URL}" \
  "${CHAIN_ID}" \
  "${PROTOCOL_VERSION}" \
  "${PUBLIC_RPC_BIND_HOST}" \
  "${PUBLIC_RPC_PORT}" \
  "${PUBLIC_P2P_ADDRESS}" \
  "${PUBLIC_P2P_PORT}" \
  "${PUBLIC_STATUS_PORT}" \
  "${PUBLIC_PROMETHEUS_PORT}" \
  "${DEBUG_RPC_BIND_HOST}" \
  "${DEBUG_RPC_PORT}" \
  "${DEBUG_P2P_ADDRESS}" \
  "${DEBUG_P2P_PORT}" \
  "${DEBUG_STATUS_PORT}" \
  "${DEBUG_PROMETHEUS_PORT}" \
  "${DEBUG_RPC_RATE_LIMITS}" \
  "${PUBLIC_RPC_RATE_LIMITS}" \
  "${SYSCOIN_EDGE_DA_COMMIT_TARGET}" <<'PY'
import base64
import json
import os
import secrets
import stat
import sys
from pathlib import Path


def q(value: str) -> str:
    return json.dumps(value)


def parse_rate_limits(value: str) -> dict[str, int]:
    out = {}
    for raw_entry in value.split(","):
        entry = raw_entry.strip()
        if not entry:
            continue
        method, sep, rps = entry.partition("=")
        if sep != "=" or not method.strip() or not rps.strip().isdigit():
            raise SystemExit(f"invalid DEBUG_RPC_RATE_LIMITS entry: {raw_entry!r}")
        out[method.strip()] = int(rps.strip())
    return out


def write_secret(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(text, encoding="utf-8")
    tmp.chmod(0o600)
    tmp.replace(path)
    path.chmod(0o600)


def read_or_create_key(path: Path) -> str:
    if path.exists():
        key = path.read_text(encoding="utf-8").strip()
        if key:
            return key
    key = "0x" + secrets.token_hex(32)
    write_secret(path, key + "\n")
    return key


def provider_lines(name: str, provider: dict) -> list[str]:
    lines = [f"{name}:"]
    for key, value in provider.items():
        if value is None:
            continue
        if isinstance(value, bool):
            rendered = "true" if value else "false"
        elif isinstance(value, (int, float)):
            rendered = str(value)
        else:
            rendered = q(str(value))
        lines.append(f"  {key}: {rendered}")
    return lines


def write_start_script(
    path: Path,
    repo: Path,
    gateway_dir: Path,
    workspace: str,
    config: Path,
    protocol: str,
    syscoin_edge_da_commit_target: str,
) -> None:
    text = f"""#!/usr/bin/env bash
set -euo pipefail
: "${{OS_SERVER_NOFILE_TARGET:=1048576}}"
: "${{OS_SERVER_NOFILE_RECOMMENDED:=131072}}"
: "${{OS_SERVER_NOFILE_MIN:=65536}}"
current_nofile="$(ulimit -n)"
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_TARGET}}" ]; then
  ulimit -n "${{OS_SERVER_NOFILE_TARGET}}" 2>/dev/null || true
fi
current_nofile="$(ulimit -n)"
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_MIN}}" ]; then
  echo "open-file limit too low for zksync-os-server: ${{current_nofile}}" >&2
  exit 1
fi
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_RECOMMENDED}}" ]; then
  echo "warning: open-file limit is below recommended value: ${{current_nofile}}" >&2
fi
export PATH="${{HOME}}/.cargo/bin:${{PATH}}"
cd {q(str(repo))}
export GATEWAY_DIR={q(str(gateway_dir))}
export ZKSYNC_OS_SERVER_PATH={q(str(repo))}
export PROTOCOL_VERSION={q(protocol)}
export SYSCOIN_EDGE_DA_COMMIT_TARGET={q(syscoin_edge_da_commit_target)}
exec bash {q(str(repo / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"))} {q(workspace)} -- run --release -- --config {q(str(config))}
"""
    path.write_text(text, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


providers = json.loads(base64.b64decode(sys.argv[1]).decode("utf-8"))
base_dir = Path(sys.argv[2])
repo = Path(sys.argv[3])
main_node_enode = sys.argv[4]
main_node_rpc_url = sys.argv[5]
chain_id = sys.argv[6]
protocol = sys.argv[7]
syscoin_edge_da_commit_target = sys.argv[22]

public = {
    "name": "zksys-public",
    "debug": False,
    "rpc_bind": sys.argv[8],
    "rpc_port": sys.argv[9],
    "p2p_address": sys.argv[10],
    "p2p_port": sys.argv[11],
    "status_port": sys.argv[12],
    "prometheus_port": sys.argv[13],
    "rate_limits": parse_rate_limits(sys.argv[21]),
}
debug = {
    "name": "zksys-debug",
    "debug": True,
    "rpc_bind": sys.argv[14],
    "rpc_port": sys.argv[15],
    "p2p_address": sys.argv[16],
    "p2p_port": sys.argv[17],
    "status_port": sys.argv[18],
    "prometheus_port": sys.argv[19],
    "rate_limits": parse_rate_limits(sys.argv[20]),
}

for instance in (public, debug):
    out_dir = base_dir / instance["name"]
    out_dir.mkdir(parents=True, exist_ok=True)
    db_dir = out_dir / "db"
    secret_key = read_or_create_key(out_dir / "network.secret")
    config_path = out_dir / "config.yaml"

    lines = [
        "general:",
        "  node_role: external",
        "  state_backend: FullDiffs",
        f"  main_node_rpc_url: {q(main_node_rpc_url)}",
        f"  rocks_db_path: {db_dir}",
        "  run_priority_tree: true",
        *provider_lines("l1_provider", providers["l1_provider"]),
        *provider_lines("gateway_provider", providers["gateway_provider"]),
        "network:",
        "  enabled: true",
        f"  secret_key: {q(secret_key)}",
        f"  address: {q(instance['p2p_address'])}",
        f"  port: {instance['p2p_port']}",
        f"  boot_nodes: {q(main_node_enode)}",
        "consensus:",
        "  enabled: false",
        "sequencer:",
        "  revm_consistency_checker_allow_bootstrap_skip: true",
        "batcher:",
        "  enabled: false",
        "prover_input_generator:",
        "  enable_input_generation: false",
        "prover_api:",
        "  enabled: false",
        f"  address: 127.0.0.1:{9000 + int(instance['rpc_port']) % 1000}",
        "  proof_storage:",
        f"    path: {db_dir / 'fri_proofs'}",
        "rpc:",
        f"  address: {instance['rpc_bind']}:{instance['rpc_port']}",
        f"  enable_debug_namespace: {'true' if instance['debug'] else 'false'}",
        "  enable_txpool_namespace: false",
        "status_server:",
        f"  address: 127.0.0.1:{instance['status_port']}",
        "observability:",
        "  prometheus:",
        f"    port: {instance['prometheus_port']}",
        "genesis:",
        f"  chain_id: {chain_id}",
    ]
    if instance["rate_limits"]:
        insert_at = lines.index("status_server:")
        # SYSCOIN: preserve the existing method=rps env-var input by mapping it to
        # upstream's tagged Tiered config. An input "*=rps" remains the global cap;
        # otherwise use a sentinel that avoids adding a practical global cap.
        max_rps = 2**32 - 1
        custom_limits = {
            method: rps for method, rps in instance["rate_limits"].items() if method != "*"
        }
        global_rps = instance["rate_limits"].get("*", max_rps)
        rate_lines = [
            "  rate_limits:",
            "    type: Tiered",
            f"    global_rps: {global_rps}",
            f"    m_rps: {max_rps}",
        ]
        if custom_limits:
            rate_lines.append("    custom_methods:")
            for method, rps in custom_limits.items():
                rate_lines.append(f"      {q(method)}: {rps}")
        lines[insert_at:insert_at] = rate_lines
    write_secret(config_path, "\n".join(lines) + "\n")
    write_start_script(
        out_dir / "start-node.sh",
        repo,
        out_dir,
        instance["name"],
        config_path,
        protocol,
        syscoin_edge_da_commit_target,
    )
PY

for instance in zksys-public zksys-debug; do
  sudo tee "/etc/systemd/system/${instance}.service" >/dev/null <<EOF
[Unit]
Description=ZKsync OS ${instance} external node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=${REMOTE_OS_SERVER_PATH}
ExecStart=${REMOTE_BASE_DIR}/${instance}/start-node.sh
Restart=always
RestartSec=10
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
done

sudo systemctl daemon-reload
sudo systemctl enable zksys-public.service zksys-debug.service
if [[ "${START_PUBLIC_SERVICE}" == "true" ]]; then
  sudo systemctl restart zksys-public.service
fi
if [[ "${START_DEBUG_SERVICE}" == "true" ]]; then
  sudo systemctl restart zksys-debug.service
fi

echo "installed zksys EN configs under ${REMOTE_BASE_DIR}"
echo "public RPC upstream: http://${PUBLIC_RPC_BIND_HOST}:${PUBLIC_RPC_PORT}"
echo "Blockscout/debug RPC upstream: http://host.docker.internal:${DEBUG_RPC_PORT} (host bind ${DEBUG_RPC_BIND_HOST}:${DEBUG_RPC_PORT})"
REMOTE_SCRIPT
