#!/usr/bin/env bash
# Generate OS-server-native chain configs from zkstack-generated gateway artifacts.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

gl_require GATEWAY_DIR
gl_require ZKSYNC_OS_SERVER_PATH

: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${EDGE_CHAIN_NAME:=zksys}"
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
: "${BITCOIN_DA_RPC_URL:=}"
: "${BITCOIN_DA_RPC_USER:=}"
: "${BITCOIN_DA_RPC_PASSWORD:=}"
: "${BITCOIN_DA_PODA_URL:=https://poda.syscoin.org}"
: "${BITCOIN_DA_WALLET_NAME:=zksync-os}"
: "${BITCOIN_DA_ADDRESS_LABEL:=zksync-os-batcher}"
: "${BITCOIN_DA_FINALITY_MODE:=Chainlock}"
: "${BITCOIN_DA_FINALITY_CONFIRMATIONS:=5}"

export GATEWAY_DIR
export ZKSYNC_OS_SERVER_PATH
export GATEWAY_CHAIN_NAME
export EDGE_CHAIN_NAME
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
export BITCOIN_DA_RPC_URL
export BITCOIN_DA_RPC_USER
export BITCOIN_DA_RPC_PASSWORD
export BITCOIN_DA_PODA_URL
export BITCOIN_DA_WALLET_NAME
export BITCOIN_DA_ADDRESS_LABEL
export BITCOIN_DA_FINALITY_MODE
export BITCOIN_DA_FINALITY_CONFIRMATIONS

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
local_dev = server_root / "local-chains" / "local_dev.yaml"

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

    wallets = load_yaml_base(source_dir / "wallets.yaml")
    operator_commit_sk = (
        wallets["blob_operator"]["private_key"]
        if pubdata_mode == "Blobs"
        else wallets["operator"]["private_key"]
    )
    operator_prove_sk = wallets["prove_operator"]["private_key"]
    operator_execute_sk = wallets["execute_operator"]["private_key"]

    config_lines = [
        "general:",
        f"  rocks_db_path: {out_dir / 'db'}",
    ]
    config_lines.extend(
        [
            "genesis:",
            f"  bridgehub_address: '{bridgehub}'",
            f"  bytecode_supplier_address: '{bytecode_supplier}'",
            f"  genesis_input_path: {out_dir / 'genesis.json'}",
            f"  chain_id: {chain_id}",
            "l1_sender:",
            f"  pubdata_mode: {pubdata_mode}",
            f"  operator_commit_sk: '{operator_commit_sk}'",
            f"  operator_prove_sk: '{operator_prove_sk}'",
            f"  operator_execute_sk: '{operator_execute_sk}'",
            "rpc:",
            f"  address: 0.0.0.0:{rpc_port}",
            "prover_api:",
            f"  address: 0.0.0.0:{prover_api_port}",
            "status_server:",
            f"  address: 0.0.0.0:{status_port}",
            "observability:",
            "  prometheus:",
            f"    port: {prometheus_port}",
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
                "external_price_api_client:",
                "  source: Forced",
                "  forced_prices:",
                "    '0x0000000000000000000000000000000000000001': 3000",
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
        write_text(
            out_dir / "pre-migration-overlay.yaml",
            "\n".join(
                [
                    "sequencer:",
                    "  max_blocks_to_produce: 0",
                    "",
                ]
            ),
        )

    shutil.copy2(source_dir / "contracts.yaml", out_dir / "contracts.yaml")
    shutil.copy2(source_dir / "wallets.yaml", out_dir / "wallets.yaml")
    shutil.copy2(source_dir / "genesis.json", out_dir / "genesis.json")

    start_script = f"""#!/usr/bin/env bash
set -euo pipefail
if [ -f "${{HOME}}/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "${{HOME}}/.cargo/env"
fi
cd "{server_root}"
exec cargo run --release -- --config "{local_dev}" --config "{out_dir / 'config.yaml'}"
"""
    if gateway_rpc_url is not None:
        start_script = f"""#!/usr/bin/env bash
set -euo pipefail
if [ -f "${{HOME}}/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "${{HOME}}/.cargo/env"
fi
cd "{server_root}"
exec cargo run --release -- --config "{local_dev}" --config "{out_dir / 'config.yaml'}" --config "{out_dir / 'gateway-overlay.yaml'}"
"""
    write_text(out_dir / "start-node.sh", start_script)
    (out_dir / "start-node.sh").chmod(0o755)

    if gateway_rpc_url is not None:
        pre_migration_start_script = f"""#!/usr/bin/env bash
set -euo pipefail
if [ -f "${{HOME}}/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "${{HOME}}/.cargo/env"
fi
cd "{server_root}"
exec cargo run --release -- --config "{local_dev}" --config "{out_dir / 'config.yaml'}" --config "{out_dir / 'pre-migration-overlay.yaml'}"
"""
        write_text(out_dir / "start-pre-migration-node.sh", pre_migration_start_script)
        (out_dir / "start-pre-migration-node.sh").chmod(0o755)


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

patch_zkstack_gateway_chain_rpc_files(
    gateway_dir,
    os.environ["GATEWAY_CHAIN_NAME"],
    int(os.environ["GATEWAY_OS_RPC_PORT"]),
)

print(output_root)
PY
