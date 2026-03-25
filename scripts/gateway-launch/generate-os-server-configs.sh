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

export GATEWAY_DIR
export ZKSYNC_OS_SERVER_PATH
export GATEWAY_CHAIN_NAME
export EDGE_CHAIN_NAME
export GATEWAY_CHAIN_ID
export EDGE_CHAIN_ID
export GATEWAY_OS_RPC_PORT
export EDGE_OS_RPC_PORT

python3 - <<'PY'
from pathlib import Path
import os
import shutil
import yaml


def load_yaml_base(path: Path):
    return yaml.load(path.read_text(), Loader=yaml.BaseLoader)


def load_yaml(path: Path):
    return yaml.safe_load(path.read_text())


def write_text(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


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
    gateway_rpc_url: str | None,
):
    source_dir = gateway_dir / "chains" / chain_name / "configs"
    if not source_dir.exists():
        return

    out_dir = output_root / chain_name
    out_dir.mkdir(parents=True, exist_ok=True)

    wallets = load_yaml(source_dir / "wallets.yaml")
    operator_commit_sk = (
        wallets["blob_operator"]["private_key"]
        if pubdata_mode == "Blobs"
        else wallets["operator"]["private_key"]
    )
    operator_prove_sk = wallets["prove_operator"]["private_key"]
    operator_execute_sk = wallets["execute_operator"]["private_key"]

    config_lines = []
    if gateway_rpc_url is not None:
        config_lines.extend(
            [
                "general:",
                f"  gateway_rpc_url: {gateway_rpc_url}",
            ]
        )
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
        ]
    )
    if pubdata_mode == "Blobs":
        config_lines.extend(
            [
                "external_price_api_client:",
                "  source: Forced",
                "  forced_prices:",
                "    '0x0000000000000000000000000000000000000001': 3000",
            ]
        )
    config_lines.append("")

    write_text(out_dir / "config.yaml", "\n".join(config_lines))

    shutil.copy2(source_dir / "contracts.yaml", out_dir / "contracts.yaml")
    shutil.copy2(source_dir / "wallets.yaml", out_dir / "wallets.yaml")
    shutil.copy2(source_dir / "genesis.json", out_dir / "genesis.json")

    start_script = f"""#!/usr/bin/env bash
set -euo pipefail
exec cargo run --release --manifest-path "{server_root / 'Cargo.toml'}" -- --config "{local_dev}" --config "{out_dir / 'config.yaml'}"
"""
    write_text(out_dir / "start-node.sh", start_script)
    (out_dir / "start-node.sh").chmod(0o755)


materialize_chain(
    chain_name=os.environ["GATEWAY_CHAIN_NAME"],
    chain_id=os.environ["GATEWAY_CHAIN_ID"],
    pubdata_mode="Blobs",
    rpc_port=os.environ["GATEWAY_OS_RPC_PORT"],
    gateway_rpc_url=None,
)

materialize_chain(
    chain_name=os.environ["EDGE_CHAIN_NAME"],
    chain_id=os.environ["EDGE_CHAIN_ID"],
    pubdata_mode="RelayedL2Calldata",
    rpc_port=os.environ["EDGE_OS_RPC_PORT"],
    gateway_rpc_url=f"http://127.0.0.1:{os.environ['GATEWAY_OS_RPC_PORT']}",
)

print(output_root)
PY
