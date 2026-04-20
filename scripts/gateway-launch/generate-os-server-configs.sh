#!/usr/bin/env bash
# Generate OS-server-native chain configs from zkstack-generated gateway artifacts.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_validate_prover_mode

gl_require GATEWAY_DIR
gl_require ZKSYNC_OS_SERVER_PATH

: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${EDGE_CHAIN_NAME:=zksys}"
: "${PROTOCOL_VERSION:=v31.0}"
: "${GATEWAY_CHAIN_ID:=57001}"
: "${EDGE_CHAIN_ID:=57057}"
: "${GATEWAY_OS_RPC_PORT:=3052}"
: "${EDGE_OS_RPC_PORT:=3050}"
: "${GATEWAY_PROVER_API_PORT:=3124}"
: "${EDGE_PROVER_API_PORT:=3125}"
: "${GATEWAY_STATUS_PORT:=3071}"
: "${EDGE_STATUS_PORT:=3072}"
: "${GATEWAY_PROMETHEUS_PORT:=3312}"
: "${EDGE_PROMETHEUS_PORT:=3313}"
: "${MATERIALIZE_EDGE_CONFIG:=true}"
: "${GATEWAY_ARCHIVE_L1_RPC_URL:=${L1_RPC_URL:-}}"
: "${BITCOIN_DA_RPC_URL:=}"
: "${BITCOIN_DA_RPC_USER:=}"
: "${BITCOIN_DA_RPC_PASSWORD:=}"
: "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
: "${BITCOIN_DA_WALLET_NAME:=zksync-os}"
: "${BITCOIN_DA_ADDRESS_LABEL:=zksync-os-batcher}"
: "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
: "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"
: "${BITCOIN_DA_COOKIE_FILE:=}"
: "${ETH_GAS_PRICE:=1gwei}"
: "${ETH_PRIORITY_GAS_PRICE:=1gwei}"

resolve_syscoin_cookie_file() {
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

COOKIE_FILE="$(resolve_syscoin_cookie_file || true)"

if [ -z "${BITCOIN_DA_RPC_URL}" ] && [ -n "${COOKIE_FILE}" ]; then
  BITCOIN_DA_RPC_URL="http://127.0.0.1:18370"
fi
if { [ -z "${BITCOIN_DA_RPC_USER}" ] || [ -z "${BITCOIN_DA_RPC_PASSWORD}" ]; } && [ -n "${COOKIE_FILE}" ]; then
  COOKIE="$(< "${COOKIE_FILE}")"
  : "${BITCOIN_DA_RPC_USER:=${COOKIE%%:*}}"
  : "${BITCOIN_DA_RPC_PASSWORD:=${COOKIE#*:}}"
fi

export GATEWAY_DIR
export ZKSYNC_OS_SERVER_PATH
export GATEWAY_CHAIN_NAME
export EDGE_CHAIN_NAME
export PROTOCOL_VERSION
export GATEWAY_CHAIN_ID
export EDGE_CHAIN_ID
export GATEWAY_OS_RPC_PORT
export EDGE_OS_RPC_PORT
export GATEWAY_PROVER_API_PORT
export EDGE_PROVER_API_PORT
export GATEWAY_STATUS_PORT
export EDGE_STATUS_PORT
export GATEWAY_PROMETHEUS_PORT
export EDGE_PROMETHEUS_PORT
export MATERIALIZE_EDGE_CONFIG
export GATEWAY_ARCHIVE_L1_RPC_URL
export BITCOIN_DA_RPC_URL
export BITCOIN_DA_RPC_USER
export BITCOIN_DA_RPC_PASSWORD
export BITCOIN_DA_PODA_URL
export BITCOIN_DA_WALLET_NAME
export BITCOIN_DA_ADDRESS_LABEL
export BITCOIN_DA_FINALITY_MODE
export BITCOIN_DA_FINALITY_CONFIRMATIONS
export ETH_GAS_PRICE
export ETH_PRIORITY_GAS_PRICE
export PROVER_MODE

python3 - <<'PY'
from pathlib import Path
import os
import re
import shutil
import yaml


def load_yaml_base(path: Path):
    return yaml.load(path.read_text(), Loader=yaml.BaseLoader)


def load_yaml(path: Path):
    return yaml.safe_load(path.read_text())


def write_text(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def parse_ether_amount_to_wei(value: str, default_wei: int) -> int:
    raw = (value or "").strip().lower()
    if not raw:
        return default_wei
    m = re.fullmatch(r"([0-9]+)(?:\s*(wei|gwei|eth|ether))?", raw)
    if not m:
        raise SystemExit(f"invalid ether amount '{value}' (examples: 1gwei, 200gwei, 1000000000)")
    amount = int(m.group(1))
    unit = m.group(2) or "wei"
    multipliers = {
        "wei": 1,
        "gwei": 10**9,
        "eth": 10**18,
        "ether": 10**18,
    }
    return amount * multipliers[unit]


def sync_zkstack_gateway_l2_rpc_in_yaml(tree, port: int) -> None:
    """Set L2 JSON-RPC port inside zkstack chain general.yaml (any nesting depth)."""
    port_s = str(port)

    def walk(obj):
        if isinstance(obj, dict):
            wr = obj.get("web3_json_rpc")
            if isinstance(wr, dict):
                if "http_port" in wr:
                    wr["http_port"] = port
                for k in ("http_url",):
                    if k in wr and isinstance(wr[k], str):
                        s = wr[k]
                        s = re.sub(r"127\.0\.0\.1:\d+", f"127.0.0.1:{port_s}", s)
                        s = re.sub(r"localhost:\d+", f"localhost:{port_s}", s)
                        wr[k] = s
            for v in obj.values():
                walk(v)
        elif isinstance(obj, list):
            for x in obj:
                walk(x)

    walk(tree)
    if isinstance(tree, dict) and isinstance(tree.get("http_rpc_url"), str):
        s = tree["http_rpc_url"]
        s = re.sub(r"127\.0\.0\.1:\d+", f"127.0.0.1:{port_s}", s)
        s = re.sub(r"localhost:\d+", f"localhost:{port_s}", s)
        tree["http_rpc_url"] = s


def patch_zkstack_gateway_chain_rpc_files(
    gateway_dir: Path, gateway_chain_name: str, port: int
) -> None:
    """Match zkstack gateway chain YAML to GATEWAY_OS_RPC_PORT.

    `zkstack chain init` writes chains/<gateway>/configs/general.yaml using the
    zkstack-era default HTTP port (3050). OS-server's JSON-RPC binds to
    GATEWAY_OS_RPC_PORT instead (e.g. 3052). Nothing updated the zkstack file, but
    `zkstack chain gateway migrate-to-gateway` reads the gateway L2 URL from that YAML
    (`l2_http_url`), so the port must match the running node.
    """
    cfg_dir = gateway_dir / "chains" / gateway_chain_name / "configs"
    gen = cfg_dir / "general.yaml"
    if gen.exists():
        data = yaml.safe_load(gen.read_text(encoding="utf-8"))
        if data is not None:
            sync_zkstack_gateway_l2_rpc_in_yaml(data, port)
            gen.write_text(
                yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
                encoding="utf-8",
            )
    ext = cfg_dir / "external_node.yaml"
    if ext.exists():
        data = yaml.safe_load(ext.read_text(encoding="utf-8"))
        if isinstance(data, dict) and isinstance(data.get("main_node_url"), str):
            port_s = str(port)
            mu = data["main_node_url"]
            mu = re.sub(r"127\.0\.0\.1:\d+", f"127.0.0.1:{port_s}", mu)
            mu = re.sub(r"localhost:\d+", f"localhost:{port_s}", mu)
            data["main_node_url"] = mu
            ext.write_text(
                yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
                encoding="utf-8",
            )


gateway_dir = Path(os.environ["GATEWAY_DIR"])
server_root = Path(os.environ["ZKSYNC_OS_SERVER_PATH"])
output_root = gateway_dir / "os-server-configs"
prover_mode = os.environ.get("PROVER_MODE", "gpu").strip().lower()
if prover_mode not in {"gpu", "no-proofs"}:
    raise SystemExit(f"invalid PROVER_MODE '{prover_mode}' (expected gpu|no-proofs)")
use_mock_prover = prover_mode == "no-proofs"
materialize_edge_config = os.environ.get("MATERIALIZE_EDGE_CONFIG", "true").strip().lower()
if materialize_edge_config not in {"true", "false"}:
    raise SystemExit("invalid MATERIALIZE_EDGE_CONFIG (expected true|false)")
l1_rpc_url = os.environ.get("GATEWAY_ARCHIVE_L1_RPC_URL", "").strip()
if not l1_rpc_url:
    raise SystemExit(
        "missing gateway runtime L1 RPC URL: set GATEWAY_ARCHIVE_L1_RPC_URL or L1_RPC_URL"
    )

eco_contracts = load_yaml_base(gateway_dir / "configs" / "contracts.yaml")
bridgehub = eco_contracts["core_ecosystem_contracts"]["bridgehub_proxy_addr"]
bytecode_supplier = eco_contracts["zksync_os_ctm"]["l1_bytecodes_supplier_addr"]


def materialize_chain(
    *,
    chain_name: str,
    chain_id: str,
    pubdata_mode: str,
    rpc_port: str,
    prover_api_port: str,
    status_port: str,
    prometheus_port: str,
    gateway_rpc_url: str | None,
):
    source_dir = gateway_dir / "chains" / chain_name / "configs"
    if not source_dir.exists():
        return
    out_dir = output_root / chain_name
    out_dir.mkdir(parents=True, exist_ok=True)

    wallets_yaml = source_dir / "wallets.yaml"
    genesis_json = source_dir / "genesis.json"
    contracts_yaml = source_dir / "contracts.yaml"

    if not wallets_yaml.exists():
        raise FileNotFoundError(f"missing wallets config under {source_dir}: expected wallets.yaml")

    contracts_candidate = contracts_yaml
    if not contracts_candidate.exists():
        # `zkstack chain create` may leave `contracts_<id>.yaml` before a canonical
        # `contracts.yaml` appears; pick the newest one as source of truth.
        contract_candidates = sorted(
            [p for p in source_dir.glob("contracts_*.yaml") if p.is_file()],
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        if contract_candidates:
            contracts_candidate = contract_candidates[0]

    if not contracts_candidate.exists() or not genesis_json.exists():
        missing = []
        if not contracts_candidate.exists():
            missing.append("contracts.yaml|contracts_*.yaml")
        if not genesis_json.exists():
            missing.append("genesis.json")
        raise FileNotFoundError(
            f"missing required chain config under {source_dir}: {', '.join(missing)}"
        )

    wallets = load_yaml_base(wallets_yaml)
    operator_commit_sk = (
        wallets["blob_operator"]["private_key"]
        if pubdata_mode == "Blobs"
        else wallets["operator"]["private_key"]
    )
    operator_prove_sk = wallets["prove_operator"]["private_key"]
    operator_execute_sk = wallets["execute_operator"]["private_key"]
    max_fee_per_gas_wei = parse_ether_amount_to_wei(
        os.environ.get("ETH_GAS_PRICE", ""),
        1 * 10**9,
    )
    max_priority_fee_per_gas_wei = parse_ether_amount_to_wei(
        os.environ.get("ETH_PRIORITY_GAS_PRICE", ""),
        1 * 10**9,
    )

    config_lines = [
        "general:",
        f"  rocks_db_path: {out_dir / 'db'}",
        f"  l1_rpc_url: '{l1_rpc_url}'",
        *(
            ["  startup_sl_finalization_timeout: 3000s"]
            if pubdata_mode != "RelayedL2Calldata"
            else []
        ),
    ]
    config_lines.extend(
        [
            "genesis:",
            f"  bridgehub_address: '{bridgehub}'",
            f"  bytecode_supplier_address: '{bytecode_supplier}'",
            f"  genesis_input_path: {out_dir / 'genesis.json'}",
            f"  chain_id: {chain_id}",
            "sequencer:",
            "  revm_consistency_checker_enabled: false",
            "l1_sender:",
            f"  pubdata_mode: {pubdata_mode}",
            f"  operator_commit_sk: '{operator_commit_sk}'",
            f"  operator_prove_sk: '{operator_prove_sk}'",
            f"  operator_execute_sk: '{operator_execute_sk}'",
            f"  max_fee_per_gas: '{max_fee_per_gas_wei} wei'",
            f"  max_priority_fee_per_gas: '{max_priority_fee_per_gas_wei} wei'",
            *(
                ["  transaction_timeout: 3000s"]
                if pubdata_mode != "RelayedL2Calldata"
                else []
            ),
            "rpc:",
            f"  address: 0.0.0.0:{rpc_port}",
            "prover_api:",
            f"  address: 0.0.0.0:{prover_api_port}",
            "  fake_fri_provers:",
            f"    enabled: {'true' if use_mock_prover else 'false'}",
            "  fake_snark_provers:",
            f"    enabled: {'true' if use_mock_prover else 'false'}",
            "  proof_storage:",
            f"    path: {out_dir / 'db' / 'fri_proofs'}",
            "status_server:",
            f"  address: 0.0.0.0:{status_port}",
            "observability:",
            "  prometheus:",
            f"    port: {prometheus_port}",
            "external_price_api_client:",
            "  source: Forced",
            "  forced_prices:",
            "    '0x0000000000000000000000000000000000000001': 3000",
        ]
    )
    if pubdata_mode == "Blobs":
        config_lines.extend(
            [
                "batcher:",
                f"  bitcoin_da_rpc_url: {os.environ['BITCOIN_DA_RPC_URL']}",
                f"  bitcoin_da_rpc_user: '{os.environ['BITCOIN_DA_RPC_USER']}'",
                f"  bitcoin_da_rpc_password: '{os.environ['BITCOIN_DA_RPC_PASSWORD']}'",
                f"  bitcoin_da_poda_url: {os.environ['BITCOIN_DA_PODA_URL']}",
                f"  bitcoin_da_wallet_name: {os.environ['BITCOIN_DA_WALLET_NAME']}",
                f"  bitcoin_da_address_label: {os.environ['BITCOIN_DA_ADDRESS_LABEL']}",
                f"  bitcoin_da_finality_mode: {os.environ['BITCOIN_DA_FINALITY_MODE']}",
                f"  bitcoin_da_finality_confirmations: {os.environ['BITCOIN_DA_FINALITY_CONFIRMATIONS']}",
            ]
        )
    config_lines.append("")

    write_text(out_dir / "config.yaml", "\n".join(config_lines))

    if gateway_rpc_url is not None:
        write_text(
            out_dir / "gateway-overlay.yaml",
            "\n".join(
                [
                    "general:",
                    f"  gateway_rpc_url: {gateway_rpc_url}",
                    "",
                ]
            ),
        )

    shutil.copy2(contracts_candidate, out_dir / "contracts.yaml")
    shutil.copy2(wallets_yaml, out_dir / "wallets.yaml")
    shutil.copy2(genesis_json, out_dir / "genesis.json")

    config_path = out_dir / "config.yaml"
    start_config_args = f'--config "{config_path}"'
    if gateway_rpc_url is not None:
        start_config_args += f' --config "{out_dir / "gateway-overlay.yaml"}"'

    refresh_cookie_block = ""
    if pubdata_mode == "Blobs":
        refresh_cookie_block = f"""
resolve_syscoin_cookie_file() {{
  local cookie_file datadir network candidate
  cookie_file="${{BITCOIN_DA_COOKIE_FILE:-}}"
  if [ -n "${{cookie_file}}" ] && [ -f "${{cookie_file}}" ]; then
    printf '%s\\n' "${{cookie_file}}"
    return 0
  fi

  datadir="${{SYSCOIN_DATADIR:-${{HOME}}/.syscoin}}"
  network="${{SYSCOIN_NETWORK:-}}"
  if [ -n "${{network}}" ]; then
    candidate="${{datadir}}/${{network}}/.cookie"
    if [ -f "${{candidate}}" ]; then
      printf '%s\\n' "${{candidate}}"
      return 0
    fi
  fi

  candidate="${{datadir}}/testnet3/.cookie"
  if [ -f "${{candidate}}" ]; then
    printf '%s\\n' "${{candidate}}"
    return 0
  fi
  return 1
}}

COOKIE_FILE="$(resolve_syscoin_cookie_file || true)"
if [ -n "${{COOKIE_FILE}}" ]; then
  COOKIE="$(< "${{COOKIE_FILE}}")"
  BITCOIN_DA_RPC_USER="${{COOKIE%%:*}}"
  BITCOIN_DA_RPC_PASSWORD="${{COOKIE#*:}}"

  python3 - "{config_path}" "${{BITCOIN_DA_RPC_USER}}" "${{BITCOIN_DA_RPC_PASSWORD}}" <<'PYCOOKIE'
import json
import re
import sys
from pathlib import Path

config_path = Path(sys.argv[1])
rpc_user = sys.argv[2]
rpc_password = sys.argv[3]
text = config_path.read_text(encoding="utf-8")
text, user_count = re.subn(
    r"^(\\s*bitcoin_da_rpc_user:\\s*).*$",
    lambda m: f"{{m.group(1)}}{{json.dumps(rpc_user)}}",
    text,
    count=1,
    flags=re.MULTILINE,
)
text, password_count = re.subn(
    r"^(\\s*bitcoin_da_rpc_password:\\s*).*$",
    lambda m: f"{{m.group(1)}}{{json.dumps(rpc_password)}}",
    text,
    count=1,
    flags=re.MULTILINE,
)
if user_count != 1 or password_count != 1:
    raise SystemExit(f"failed to patch Syscoin RPC credentials in {{config_path}}")
config_path.write_text(text, encoding="utf-8")
print(f"gateway-launch: refreshed Syscoin RPC credentials in {{config_path}}")
PYCOOKIE
else
  echo "gateway-launch: Syscoin cookie not found; using existing credentials in {config_path}" >&2
fi
"""

    start_script = f"""#!/usr/bin/env bash
set -euo pipefail
if [ -f "${{HOME}}/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "${{HOME}}/.cargo/env"
fi
: "${{OS_SERVER_NOFILE_TARGET:=1048576}}"
: "${{OS_SERVER_NOFILE_RECOMMENDED:=131072}}"
: "${{OS_SERVER_NOFILE_MIN:=65536}}"
current_nofile="$(ulimit -n)"
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_TARGET}}" ]; then
  ulimit -n "${{OS_SERVER_NOFILE_TARGET}}" 2>/dev/null || true
fi
current_nofile="$(ulimit -n)"
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_MIN}}" ]; then
  echo "gateway-launch: open-file limit too low for os-server: ${{current_nofile}} (need at least ${{OS_SERVER_NOFILE_MIN}}, target ${{OS_SERVER_NOFILE_TARGET}})" >&2
  echo "gateway-launch: raise the shell/system nofile hard limit and retry" >&2
  exit 1
fi
if [ "${{current_nofile}}" -lt "${{OS_SERVER_NOFILE_RECOMMENDED}}" ]; then
  echo "gateway-launch: warning: open-file limit is below recommended value: ${{current_nofile}} < ${{OS_SERVER_NOFILE_RECOMMENDED}}" >&2
fi
cd "{server_root}"
export GATEWAY_DIR="{gateway_dir}"
export PROTOCOL_VERSION="{os.environ["PROTOCOL_VERSION"]}"{refresh_cookie_block}
export ETH_GAS_PRICE="{os.environ["ETH_GAS_PRICE"]}"
export ETH_PRIORITY_GAS_PRICE="{os.environ["ETH_PRIORITY_GAS_PRICE"]}"
exec bash "{server_root / 'scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh'}" "{chain_name}" -- run --release -- {start_config_args}
"""
    write_text(out_dir / "start-node.sh", start_script)
    (out_dir / "start-node.sh").chmod(0o755)

materialize_chain(
    chain_name=os.environ["GATEWAY_CHAIN_NAME"],
    chain_id=os.environ["GATEWAY_CHAIN_ID"],
    pubdata_mode="Blobs",
    rpc_port=os.environ["GATEWAY_OS_RPC_PORT"],
    prover_api_port=os.environ["GATEWAY_PROVER_API_PORT"],
    status_port=os.environ["GATEWAY_STATUS_PORT"],
    prometheus_port=os.environ["GATEWAY_PROMETHEUS_PORT"],
    gateway_rpc_url=None,
)

if materialize_edge_config == "true":
    materialize_chain(
        chain_name=os.environ["EDGE_CHAIN_NAME"],
        chain_id=os.environ["EDGE_CHAIN_ID"],
        pubdata_mode="RelayedL2Calldata",
        rpc_port=os.environ["EDGE_OS_RPC_PORT"],
        prover_api_port=os.environ["EDGE_PROVER_API_PORT"],
        status_port=os.environ["EDGE_STATUS_PORT"],
        prometheus_port=os.environ["EDGE_PROMETHEUS_PORT"],
        gateway_rpc_url=f"http://127.0.0.1:{os.environ['GATEWAY_OS_RPC_PORT']}",
    )
else:
    print("gateway-launch: skipping edge OS-server config materialization for this phase")

patch_zkstack_gateway_chain_rpc_files(
    gateway_dir,
    os.environ["GATEWAY_CHAIN_NAME"],
    int(os.environ["GATEWAY_OS_RPC_PORT"]),
)

print(output_root)
PY
