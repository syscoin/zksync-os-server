#!/usr/bin/env bash
# Generate OS-server-native chain configs from zkstack-generated gateway artifacts.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_validate_prover_mode
gl_reject_no_proofs_on_mainnet

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
: "${PROVER_API_BIND_HOST:=127.0.0.1}"
: "${GATEWAY_PROVER_API_DOMAIN:=prover-gw.dev11.top}"
: "${EDGE_PROVER_API_DOMAIN:=prover-zk.dev11.top}"
: "${PROVER_API_AUTH_PASSWORD:=}"
: "${PROVER_API_AUTH_USER:=${PROVER_API_AUTH_PASSWORD:+syscoin-prover}}"
: "${GATEWAY_BLOCK_PUBDATA_LIMIT_BYTES:=67108833}"
: "${GATEWAY_BATCH_TIMEOUT:=1000s}"
# SYSCOIN: Keep the generated edge limit aligned with one Syscoin DA blob and
# the node sequencer default so valid priority operations cannot wedge the queue.
: "${EDGE_BLOCK_PUBDATA_LIMIT_BYTES:=2097152}"
: "${EDGE_BLOCK_TIME:=2s}"
: "${EDGE_PUBDATA_PRICING_MULTIPLIER:=32.0}"
# SYSCOIN: Gateway's base token is TSYS in this launch. The upstream default
# native resource price targets ETH-like base tokens and yields ~30k gwei here.
: "${GATEWAY_NATIVE_PER_GAS:=100}"
: "${GATEWAY_NATIVE_PRICE_USD:=3e-12}"
: "${EDGE_NATIVE_PER_GAS:=100}"
: "${EDGE_NATIVE_PRICE_USD:=3e-11}"
: "${NATIVE_TOKEN_PRICE_USD:=0.01}"
: "${MATERIALIZE_EDGE_CONFIG:=true}"
L1_RPC_URL_WAS_SET=false
GATEWAY_ARCHIVE_L1_RPC_URL_WAS_SET=false
if [ -n "${L1_RPC_URL+x}" ]; then
  L1_RPC_URL_WAS_SET=true
fi
if [ -n "${GATEWAY_ARCHIVE_L1_RPC_URL+x}" ]; then
  GATEWAY_ARCHIVE_L1_RPC_URL_WAS_SET=true
fi
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

if [ "${GATEWAY_ARCHIVE_L1_RPC_URL_WAS_SET}" = false ] &&
  [ "${L1_RPC_URL_WAS_SET}" = false ] &&
  [ -f "${GATEWAY_DIR}/os-server-configs/${GATEWAY_CHAIN_NAME}/config.yaml" ]; then
  # SYSCOIN: checkpoint repairs may regenerate configs without RPC env. Preserve
  # both materialized L1 RPCs only in that case. If L1_RPC_URL or
  # GATEWAY_ARCHIVE_L1_RPC_URL is supplied, the explicit value wins.
  existing_l1_rpc_urls="$(python3 - "${GATEWAY_DIR}/os-server-configs/${GATEWAY_CHAIN_NAME}/config.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

p = Path(sys.argv[1])
data = yaml.safe_load(p.read_text(encoding="utf-8"))
archive_provider = data.get("l1_archive_provider") if isinstance(data, dict) else None
l1_provider = data.get("l1_provider") if isinstance(data, dict) else None

def rpc_url(provider):
    if not isinstance(provider, dict):
        return ""
    url = provider.get("rpc_url")
    if isinstance(url, str) and url.strip():
        return url.strip()
    return ""

runtime_url = rpc_url(l1_provider)
archive_url = rpc_url(archive_provider) or runtime_url
print(runtime_url)
print(archive_url)
PY
)"
  existing_runtime_l1_rpc_url="$(printf '%s\n' "${existing_l1_rpc_urls}" | sed -n '1p')"
  existing_archive_l1_rpc_url="$(printf '%s\n' "${existing_l1_rpc_urls}" | sed -n '2p')"
  if [ -n "${existing_runtime_l1_rpc_url}" ]; then
    L1_RPC_URL="${existing_runtime_l1_rpc_url}"
  fi
  if [ -n "${existing_archive_l1_rpc_url}" ]; then
    GATEWAY_ARCHIVE_L1_RPC_URL="${existing_archive_l1_rpc_url}"
  fi
fi

if [ -z "${PROVER_API_AUTH_PASSWORD}" ] && [ -f "${GATEWAY_DIR}/os-server-configs/${GATEWAY_CHAIN_NAME}/config.yaml" ]; then
  # SYSCOIN: final config regeneration can run after the Gateway config has
  # already been materialized. Reuse its prover API credentials on checkpointed
  # reruns so operators do not need to re-export secrets just to create the edge
  # config.
  existing_prover_api_auth="$(python3 - "${GATEWAY_DIR}/os-server-configs/${GATEWAY_CHAIN_NAME}/config.yaml" <<'PY'
import sys
from pathlib import Path

import yaml

p = Path(sys.argv[1])
data = yaml.safe_load(p.read_text(encoding="utf-8"))
prover_api = data.get("prover_api") if isinstance(data, dict) else None
if isinstance(prover_api, dict):
    user = prover_api.get("auth_user")
    password = prover_api.get("auth_password")
    if isinstance(user, str) and user.strip() and isinstance(password, str) and password.strip():
        print(user.strip())
        print(password.strip())
PY
)"
  if [ -n "${existing_prover_api_auth}" ]; then
    PROVER_API_AUTH_USER="$(printf '%s\n' "${existing_prover_api_auth}" | sed -n '1p')"
    PROVER_API_AUTH_PASSWORD="$(printf '%s\n' "${existing_prover_api_auth}" | sed -n '2p')"
  fi
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
export PROVER_API_BIND_HOST
export GATEWAY_PROVER_API_DOMAIN
export EDGE_PROVER_API_DOMAIN
export PROVER_API_AUTH_USER
export PROVER_API_AUTH_PASSWORD
export GATEWAY_BLOCK_PUBDATA_LIMIT_BYTES
export GATEWAY_BATCH_TIMEOUT
export EDGE_BLOCK_PUBDATA_LIMIT_BYTES
export EDGE_BLOCK_TIME
export EDGE_PUBDATA_PRICING_MULTIPLIER
export GATEWAY_NATIVE_PER_GAS
export GATEWAY_NATIVE_PRICE_USD
export EDGE_NATIVE_PER_GAS
export EDGE_NATIVE_PRICE_USD
export NATIVE_TOKEN_PRICE_USD
export MATERIALIZE_EDGE_CONFIG
export L1_RPC_URL
export GATEWAY_ARCHIVE_L1_RPC_URL
export BITCOIN_DA_RPC_URL
export BITCOIN_DA_RPC_USER
export BITCOIN_DA_RPC_PASSWORD
export BITCOIN_DA_PODA_URL
export BITCOIN_DA_WALLET_NAME
export BITCOIN_DA_ADDRESS_LABEL
export BITCOIN_DA_FINALITY_MODE
export BITCOIN_DA_FINALITY_CONFIRMATIONS
export PROVER_MODE

python3 - <<'PY'
from pathlib import Path
import json
import os
import re
import shutil
import tempfile
import yaml


def load_yaml_base(path: Path):
    return yaml.load(path.read_text(), Loader=yaml.BaseLoader)


def load_yaml(path: Path):
    return yaml.safe_load(path.read_text())


def normalize_nonzero_address(value: str, label: str) -> str:
    if not isinstance(value, str):
        raise SystemExit(f"{label} must be a 20-byte hex address")
    address = value.strip().lower()
    if not re.fullmatch(r"0x[0-9a-f]{40}", address):
        raise SystemExit(f"{label} must be a 20-byte hex address")
    if address == "0x" + "0" * 40:
        raise SystemExit(f"{label} must be nonzero")
    return address


def write_text(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def remove_if_exists(path: Path):
    path.unlink(missing_ok=True)


def write_secret_text(path: Path, text: str, mode: int = 0o600):
    # SYSCOIN: generated OS server configs contain operator private keys; create
    # them independent of the caller's umask so staging artifacts are not world-readable.
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.",
            delete=False,
        ) as tmp:
            tmp_path = Path(tmp.name)
            tmp.write(text)
        tmp_path.chmod(mode)
        os.replace(tmp_path, path)
        path.chmod(mode)
    except BaseException:
        if tmp_path is not None:
            tmp_path.unlink(missing_ok=True)
        raise


def yaml_scalar(value: str) -> str:
    return json.dumps(value)


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
runtime_l1_rpc_url = os.environ.get("L1_RPC_URL", "").strip()
archive_l1_rpc_url = os.environ.get("GATEWAY_ARCHIVE_L1_RPC_URL", "").strip()
if not runtime_l1_rpc_url:
    runtime_l1_rpc_url = archive_l1_rpc_url
if not archive_l1_rpc_url:
    archive_l1_rpc_url = runtime_l1_rpc_url
if not runtime_l1_rpc_url:
    raise SystemExit(
        "missing gateway runtime L1 RPC URL: set L1_RPC_URL or GATEWAY_ARCHIVE_L1_RPC_URL"
    )
prover_api_auth_user = os.environ.get("PROVER_API_AUTH_USER", "").strip()
prover_api_auth_password = os.environ.get("PROVER_API_AUTH_PASSWORD", "").strip()
prover_api_bind_host = os.environ.get("PROVER_API_BIND_HOST", "").strip()
if not prover_api_bind_host:
    raise SystemExit("missing prover API bind host: set PROVER_API_BIND_HOST")
prover_api_bind_is_loopback = prover_api_bind_host in {"127.0.0.1", "localhost", "::1"}
if (not prover_api_auth_user or not prover_api_auth_password) and (
    not use_mock_prover or not prover_api_bind_is_loopback
):
    raise SystemExit(
        "missing prover API credentials: set PROVER_API_AUTH_USER and PROVER_API_AUTH_PASSWORD"
    )
prover_api_auth_config_lines = []
if prover_api_auth_user and prover_api_auth_password:
    prover_api_auth_config_lines = [
        f"  auth_user: {yaml_scalar(prover_api_auth_user)}",
        f"  auth_password: {yaml_scalar(prover_api_auth_password)}",
    ]
prover_api_nginx_enabled = bool(prover_api_auth_config_lines)

eco_contracts = load_yaml_base(gateway_dir / "configs" / "contracts.yaml")
bridgehub = eco_contracts["core_ecosystem_contracts"]["bridgehub_proxy_addr"]
bytecode_supplier = eco_contracts["zksync_os_ctm"]["l1_bytecodes_supplier_addr"]


def nginx_prover_api_server_block(domain: str, prover_api_port: str) -> str:
    domain = domain.strip()
    if not domain:
        raise SystemExit("missing prover API nginx domain")
    return f"""server {{
    listen 80;
    listen [::]:80;
    server_name {domain};
    return 301 https://$host$request_uri;
}}

server {{
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name {domain};

    ssl_certificate /etc/letsencrypt/live/{domain}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{domain}/privkey.pem;
    ssl_trusted_certificate /etc/letsencrypt/live/{domain}/chain.pem;

    # Airbender proof submissions can exceed nginx's 1M default.
    client_max_body_size 0;

    location / {{
        proxy_pass http://127.0.0.1:{prover_api_port};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
    }}
}}
"""


def materialize_chain(
    *,
    chain_name: str,
    chain_id: str,
    pubdata_mode: str,
    rpc_port: str,
    prover_api_port: str,
    status_port: str,
    prometheus_port: str,
    block_pubdata_limit_bytes: str,
    gateway_rpc_url: str | None,
    prover_api_domain: str,
):
    # SYSCOIN: edge chains in RelayedL2Calldata mode publish pubdata directly to Bitcoin DA and
    # relay only compact blob hashes to Gateway, so they need the same DA plumbing as Blobs mode.
    uses_syscoin_da_refs = pubdata_mode in ("Blobs", "RelayedL2Calldata")
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

    contracts_source = contracts_yaml
    if not contracts_source.exists():
        # SYSCOIN: zkstack may emit contracts_<chain-id>.yaml before canonical
        # contracts.yaml; only accept the file matching this chain's expected ID.
        contracts_source = source_dir / f"contracts_{chain_id}.yaml"

    if not contracts_source.exists() or not genesis_json.exists():
        missing = []
        if not contracts_source.exists():
            missing.append(f"contracts.yaml|contracts_{chain_id}.yaml")
        if not genesis_json.exists():
            missing.append("genesis.json")
        raise FileNotFoundError(
            f"missing required chain config under {source_dir}: {', '.join(missing)}"
        )

    chain_contracts = load_yaml_base(contracts_source)
    wallets = load_yaml_base(wallets_yaml)
    operator_commit_sk = (
        wallets["blob_operator"]["private_key"]
        if uses_syscoin_da_refs
        else wallets["operator"]["private_key"]
    )
    operator_prove_sk = wallets["prove_operator"]["private_key"]
    operator_execute_sk = wallets["execute_operator"]["private_key"]
    fee_collector_address = wallets["fee_account"]["address"]
    expected_fee_recipient_address = "0x0000000000000000000000000000000000000000"
    if chain_name == os.environ["EDGE_CHAIN_NAME"]:
        l2_contracts = chain_contracts.get("l2") if isinstance(chain_contracts, dict) else None
        if not isinstance(l2_contracts, dict) or l2_contracts.get("zksys_fee_collector_addr") is None:
            raise ValueError(
                f"{contracts_source}: l2.zksys_fee_collector_addr is required for {chain_name}"
            )
        fee_collector_address = normalize_nonzero_address(
            l2_contracts["zksys_fee_collector_addr"],
            f"{contracts_source}: l2.zksys_fee_collector_addr",
        )
        expected_fee_recipient_address = fee_collector_address

    config_lines = [
        "general:",
        f"  rocks_db_path: {out_dir / 'db'}",
        *(
            ["  startup_sl_finalization_timeout: 3000s"]
            if pubdata_mode != "RelayedL2Calldata"
            else []
        ),
        "l1_provider:",
        f"  rpc_url: '{runtime_l1_rpc_url}'",
        "l1_archive_provider:",
        f"  rpc_url: '{archive_l1_rpc_url}'",
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
            f"  block_pubdata_limit_bytes: {block_pubdata_limit_bytes}",
            f"  fee_collector_address: '{fee_collector_address}'",
            f"  expected_fee_recipient_address: '{expected_fee_recipient_address}'",
            *(
                [f"  block_time: {os.environ['EDGE_BLOCK_TIME']}"]
                if chain_name == "zksys"
                else []
            ),
            "l1_sender:",
            *(
                [f"  pubdata_mode: {pubdata_mode}"]
                if gateway_rpc_url is None
                else []
            ),
            f"  operator_commit_sk: '{operator_commit_sk}'",
            f"  operator_prove_sk: '{operator_prove_sk}'",
            f"  operator_execute_sk: '{operator_execute_sk}'",
            # SYSCOIN: commit batches are state-dependent on both settlement
            # layers, so keep L1 submissions single-flight for now.
            "  command_limit: 1",
            *(
                [
                    "  transaction_timeout: 3000s",
                    "  gateway_da_admission_retry_timeout: 90m",
                    "  gateway_da_admission_retry_interval: 30s",
                ]
                if pubdata_mode != "RelayedL2Calldata"
                else []
            ),
            *(
                [
                    "gas_adjuster:",
                    f"  pubdata_pricing_multiplier: {os.environ['EDGE_PUBDATA_PRICING_MULTIPLIER']}",
                ]
                if pubdata_mode == "RelayedL2Calldata"
                else []
            ),
            *(
                [
                    "fee:",
                    f"  native_per_gas: {os.environ['GATEWAY_NATIVE_PER_GAS']}",
                    f"  native_price_usd: {os.environ['GATEWAY_NATIVE_PRICE_USD']}",
                ]
                if chain_name == "gateway"
                else []
            ),
            *(
                [
                    "gateway_sender:",
                    f"  operator_commit_sk: '{operator_commit_sk}'",
                    f"  operator_prove_sk: '{operator_prove_sk}'",
                    f"  operator_execute_sk: '{operator_execute_sk}'",
                    # SYSCOIN: Gateway-settled child chains still submit state-dependent
                    # settlement transactions; keep the same single-flight discipline as L1.
                    "  command_limit: 1",
                    "  gateway_da_admission_retry_timeout: 90m",
                    "  gateway_da_admission_retry_interval: 30s",
                ]
                if gateway_rpc_url is not None
                else []
            ),
            *(
                [
                    "fee:",
                    f"  native_per_gas: {os.environ['EDGE_NATIVE_PER_GAS']}",
                    f"  native_price_usd: {os.environ['EDGE_NATIVE_PRICE_USD']}",
                ]
                if chain_name == "zksys"
                else []
            ),
            "rpc:",
            f"  address: 0.0.0.0:{rpc_port}",
            *(
                [
                    "prover_input_generator:",
                    "  enable_input_generation: false",
                ]
                if use_mock_prover
                else []
            ),
            "prover_api:",
            *(
                ["  enabled: false"]
                if use_mock_prover
                else []
            ),
            f"  address: {prover_api_bind_host}:{prover_api_port}",
            *prover_api_auth_config_lines,
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
            f"    '0x0000000000000000000000000000000000000001': {os.environ['NATIVE_TOKEN_PRICE_USD']}",
        ]
    )
    if uses_syscoin_da_refs:
        config_lines.extend(
            [
                "batcher:",
                *(
                    [f"  batch_timeout: {os.environ['GATEWAY_BATCH_TIMEOUT']}"]
                    if chain_name == "gateway"
                    else []
                ),
                f"  bitcoin_da_rpc_url: {os.environ['BITCOIN_DA_RPC_URL']}",
                f"  bitcoin_da_rpc_user: '{os.environ['BITCOIN_DA_RPC_USER']}'",
                f"  bitcoin_da_rpc_password: '{os.environ['BITCOIN_DA_RPC_PASSWORD']}'",
                f"  bitcoin_da_poda_url: {os.environ['BITCOIN_DA_PODA_URL']}",
                f"  bitcoin_da_wallet_name: {os.environ['BITCOIN_DA_WALLET_NAME']}",
                f"  bitcoin_da_address_label: {os.environ['BITCOIN_DA_ADDRESS_LABEL']}",
                f"  bitcoin_da_finality_mode: {os.environ['BITCOIN_DA_FINALITY_MODE']}",
                f"  bitcoin_da_finality_confirmations: {os.environ['BITCOIN_DA_FINALITY_CONFIRMATIONS']}",
                f"  bitcoin_da_gateway_l1_republish_enabled: {os.environ.get('BITCOIN_DA_GATEWAY_L1_REPUBLISH_ENABLED', 'true')}",
            ]
        )
    if gateway_rpc_url is not None:
        config_lines.extend(
            [
                "gateway_provider:",
                f"  rpc_url: {gateway_rpc_url}",
            ]
        )
    config_lines.append("")

    write_secret_text(out_dir / "config.yaml", "\n".join(config_lines))

    shutil.copy2(contracts_source, out_dir / "contracts.yaml")
    shutil.copy2(wallets_yaml, out_dir / "wallets.yaml")
    # SYSCOIN: wallets.yaml is copied for operator convenience but still carries
    # private keys, so force the generated copy to owner-only permissions.
    (out_dir / "wallets.yaml").chmod(0o600)
    shutil.copy2(genesis_json, out_dir / "genesis.json")

    config_path = out_dir / "config.yaml"
    start_config_args = f'--config "{config_path}"'

    refresh_cookie_block = ""
    if uses_syscoin_da_refs:
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
  # SYSCOIN: keep cookie-derived credentials out of process argv.
  python3 - "{config_path}" "${{COOKIE_FILE}}" <<'PYCOOKIE'
import json
import re
import sys
from pathlib import Path

config_path = Path(sys.argv[1])
cookie_path = Path(sys.argv[2])
cookie = cookie_path.read_text(encoding="utf-8").rstrip("\\r\\n")
rpc_user, separator, rpc_password = cookie.partition(":")
if separator != ":" or not rpc_user or not rpc_password:
    raise SystemExit(f"invalid Syscoin RPC cookie format in {{cookie_path}}")
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
# SYSCOIN: do not source HOME-relative Cargo env files in generated node
# start scripts. The wrapped runner sources _common.sh, which prepends
# ~/.cargo/bin to PATH without executing shell code from HOME.
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
export GATEWAY_CHAIN_NAME="{os.environ["GATEWAY_CHAIN_NAME"]}"
export EDGE_CHAIN_NAME="{os.environ["EDGE_CHAIN_NAME"]}"
export PROTOCOL_VERSION="{os.environ["PROTOCOL_VERSION"]}"{refresh_cookie_block}
exec bash "{server_root / 'scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh'}" "{chain_name}" -- run --release -- {start_config_args}
"""
    write_text(out_dir / "start-node.sh", start_script)
    (out_dir / "start-node.sh").chmod(0o755)

    if prover_api_nginx_enabled:
        write_text(
            out_dir / "prover-api.nginx.conf",
            nginx_prover_api_server_block(prover_api_domain, prover_api_port),
        )
    else:
        remove_if_exists(out_dir / "prover-api.nginx.conf")
    stale_proxy_helper = out_dir / "start-prover-api-proxy.sh"
    if stale_proxy_helper.exists():
        stale_proxy_helper.unlink()

materialize_chain(
    chain_name=os.environ["GATEWAY_CHAIN_NAME"],
    chain_id=os.environ["GATEWAY_CHAIN_ID"],
    pubdata_mode="Blobs",
    rpc_port=os.environ["GATEWAY_OS_RPC_PORT"],
    prover_api_port=os.environ["GATEWAY_PROVER_API_PORT"],
    status_port=os.environ["GATEWAY_STATUS_PORT"],
    prometheus_port=os.environ["GATEWAY_PROMETHEUS_PORT"],
    block_pubdata_limit_bytes=os.environ["GATEWAY_BLOCK_PUBDATA_LIMIT_BYTES"],
    gateway_rpc_url=None,
    prover_api_domain=os.environ["GATEWAY_PROVER_API_DOMAIN"],
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
        block_pubdata_limit_bytes=os.environ["EDGE_BLOCK_PUBDATA_LIMIT_BYTES"],
        gateway_rpc_url=f"http://127.0.0.1:{os.environ['GATEWAY_OS_RPC_PORT']}",
        prover_api_domain=os.environ["EDGE_PROVER_API_DOMAIN"],
    )
    if prover_api_nginx_enabled:
        combined_nginx = (
            nginx_prover_api_server_block(
                os.environ["GATEWAY_PROVER_API_DOMAIN"],
                os.environ["GATEWAY_PROVER_API_PORT"],
            )
            + "\n"
            + nginx_prover_api_server_block(
                os.environ["EDGE_PROVER_API_DOMAIN"],
                os.environ["EDGE_PROVER_API_PORT"],
            )
        )
        write_text(output_root / "prover-api.nginx.conf", combined_nginx)
        install_script = f"""#!/usr/bin/env bash
set -euo pipefail
conf_src="{output_root / 'prover-api.nginx.conf'}"
if [ -n "${{NGINX_PROVER_API_CONF:-}}" ]; then
  conf_dst="${{NGINX_PROVER_API_CONF}}"
elif [ -d /etc/nginx/sites-available ]; then
  conf_dst="/etc/nginx/sites-available/zksync-os-prover-api.conf"
elif [ -d /etc/nginx/conf.d ]; then
  conf_dst="/etc/nginx/conf.d/zksync-os-prover-api.conf"
elif [ -d /etc/nginx/sites-enabled ]; then
  conf_dst="/etc/nginx/sites-enabled/zksync-os-prover-api.conf"
else
  echo "could not determine nginx config directory; set NGINX_PROVER_API_CONF" >&2
  exit 1
fi
if [ -n "${{NGINX_PROVER_API_ENABLED:-}}" ]; then
  enabled_dst="${{NGINX_PROVER_API_ENABLED}}"
elif [ -d /etc/nginx/sites-available ] && [ -d /etc/nginx/sites-enabled ] && [ "${{conf_dst}}" = "/etc/nginx/sites-available/zksync-os-prover-api.conf" ]; then
  enabled_dst="/etc/nginx/sites-enabled/$(basename "${{conf_dst}}")"
else
  enabled_dst="${{conf_dst}}"
fi

if [ ! -f "${{conf_src}}" ]; then
  echo "missing generated nginx config: ${{conf_src}}" >&2
  exit 1
fi

echo "Installing ${{conf_src}} -> ${{conf_dst}}"
sudo cp "${{conf_src}}" "${{conf_dst}}"
if [ "${{conf_dst}}" != "${{enabled_dst}}" ]; then
  sudo ln -sfn "${{conf_dst}}" "${{enabled_dst}}"
fi
sudo nginx -t
sudo systemctl reload nginx
"""
        write_text(output_root / "install-prover-api-nginx.sh", install_script)
        (output_root / "install-prover-api-nginx.sh").chmod(0o755)
    else:
        remove_if_exists(output_root / "prover-api.nginx.conf")
        remove_if_exists(output_root / "install-prover-api-nginx.sh")
else:
    print("gateway-launch: skipping edge OS-server config materialization for this phase")

patch_zkstack_gateway_chain_rpc_files(
    gateway_dir,
    os.environ["GATEWAY_CHAIN_NAME"],
    int(os.environ["GATEWAY_OS_RPC_PORT"]),
)

print(output_root)
PY

if [ -n "${BITCOIN_DA_RPC_URL}" ]; then
  gl_prepare_bitcoin_da_wallet
fi
