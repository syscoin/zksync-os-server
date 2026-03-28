# shellcheck shell=bash
# Source-only: shared paths and helpers for gateway-launch/*.sh
# shellcheck disable=SC2034
GL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZKSYNC_OS_SERVER_PATH="${ZKSYNC_OS_SERVER_PATH:-$(cd "${GL_DIR}/../.." && pwd)}"

# Ensure required CLI tooling is discoverable in non-interactive shells.
for _tool_dir in "${HOME}/.foundry/bin" "${HOME}/.cargo/bin"; do
  if [ -d "${_tool_dir}" ] && [[ ":${PATH}:" != *":${_tool_dir}:"* ]]; then
    PATH="${_tool_dir}:${PATH}"
  fi
done
export PATH
: "${PROVER_MODE:=gpu}"
export PROVER_MODE

gl_die() {
  echo "gateway-launch: $*" >&2
  exit 1
}

gl_validate_prover_mode() {
  case "${PROVER_MODE,,}" in
  gpu | mock) ;;
  *)
    gl_die "invalid PROVER_MODE='${PROVER_MODE}' (expected: gpu | mock)"
    ;;
  esac
  PROVER_MODE="${PROVER_MODE,,}"
  export PROVER_MODE
}

gl_require() {
  local n="$1"
  [ -n "${!n:-}" ] || gl_die "unset required env: $n"
}

gl_sha_from_versions() {
  gl_require PROTOCOL_VERSION
  local key="$1"
  local vf="${ZKSYNC_OS_SERVER_PATH}/local-chains/${PROTOCOL_VERSION}/versions.yaml"
  [ -f "$vf" ] || gl_die "missing ${vf}"
  VERSIONS_YAML="$vf" VERSIONS_KEY="$key" python3 - <<'PY'
import os, re

text = open(os.environ["VERSIONS_YAML"], "r", encoding="utf-8").read()
key = re.escape(os.environ["VERSIONS_KEY"])
m = re.search(
    rf"{key}:\s*(?:\n\s*#.*)*\n\s*sha:\s*\"([0-9a-f]{{40}})\"",
    text,
)
if not m:
    raise SystemExit(f"{os.environ['VERSIONS_KEY']} sha not found in versions.yaml")
print(m.group(1))
PY
}

gl_contracts_sha_from_versions() {
  gl_sha_from_versions "era-contracts"
}

gl_zkstack_cli_sha_from_versions() {
  gl_sha_from_versions "zkstack-cli"
}

gl_assert_contracts_sha() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_CONTRACTS_SHA
  local head
  head="$(git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD)"
  [ "$head" = "${REQUIRED_CONTRACTS_SHA}" ] ||
    gl_die "contracts HEAD ${head} != REQUIRED_CONTRACTS_SHA ${REQUIRED_CONTRACTS_SHA}"
}

gl_checkout_contracts_sha() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_CONTRACTS_SHA
  git -C "${ZKSYNC_ERA_PATH}" submodule update --init contracts
  git -C "${ZKSYNC_ERA_PATH}/contracts" fetch origin "${REQUIRED_CONTRACTS_SHA}"
  git -C "${ZKSYNC_ERA_PATH}/contracts" checkout "${REQUIRED_CONTRACTS_SHA}"
  git -C "${ZKSYNC_ERA_PATH}/contracts" submodule sync --recursive
  git -C "${ZKSYNC_ERA_PATH}/contracts" submodule update --init --recursive
}

gl_assert_zksync_era_sha() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  local head
  head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD)"
  if [ "$head" = "${REQUIRED_ZKSTACK_CLI_SHA}" ]; then
    return 0
  fi
  git -C "${ZKSYNC_ERA_PATH}" merge-base --is-ancestor "${REQUIRED_ZKSTACK_CLI_SHA}" "${head}" ||
    gl_die "zksync-era HEAD ${head} is not based on REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA}"
  local committed_delta
  committed_delta="$(git -C "${ZKSYNC_ERA_PATH}" diff --name-only "${REQUIRED_ZKSTACK_CLI_SHA}..${head}")"
  [ "${committed_delta}" = "contracts" ] ||
    gl_die "zksync-era HEAD ${head} differs from REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA} outside contracts: ${committed_delta}"
}

# Nightly toolchain for zkstack_cli (same discovery as preflight-zkstack-cli.sh).
gl_detect_gateway_zkstack_nightly() {
  if command -v rustup >/dev/null 2>&1; then
    rustup toolchain list | awk '/^nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}/ {print $1}' | sort -V | tail -n 1
  fi
}

# Apply Syscoin patch and build repo-local zkstack (release).
gl_build_zkstack_cli_release() {
  gl_require ZKSYNC_ERA_PATH
  gl_require ZKSYNC_OS_SERVER_PATH
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-era-syscoin-patch.sh" "${ZKSYNC_ERA_PATH}"
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env" >/dev/null 2>&1 || true
  local toolchain
  toolchain="${GATEWAY_ZKSTACK_CARGO_TOOLCHAIN:-$(gl_detect_gateway_zkstack_nightly)}"
  [ -n "${toolchain}" ] || gl_die "no nightly Rust toolchain found; install one with rustup"
  (cd "${ZKSYNC_ERA_PATH}/zkstack_cli" && cargo +"${toolchain}" build --release --locked -Znext-lockfile-bump -p zkstack)
}

gl_prepare_zksync_era_repo() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  local url="${ZKSYNC_ERA_GIT_URL:-https://github.com/matter-labs/zksync-era.git}"

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    mkdir -p "$(dirname "${ZKSYNC_ERA_PATH}")"
    git clone "${url}" "${ZKSYNC_ERA_PATH}"
  fi

  local current_head
  current_head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD)"
  if ! git -C "${ZKSYNC_ERA_PATH}" merge-base --is-ancestor "${REQUIRED_ZKSTACK_CLI_SHA}" "${current_head}" 2>/dev/null; then
    if [ -n "$(git -C "${ZKSYNC_ERA_PATH}" status --porcelain)" ]; then
      gl_die "zksync-era has local changes; cannot check out REQUIRED_ZKSTACK_CLI_SHA ${REQUIRED_ZKSTACK_CLI_SHA}"
    fi
    git -C "${ZKSYNC_ERA_PATH}" fetch origin "${REQUIRED_ZKSTACK_CLI_SHA}"
    git -C "${ZKSYNC_ERA_PATH}" checkout "${REQUIRED_ZKSTACK_CLI_SHA}"
  fi

  gl_checkout_contracts_sha
  gl_assert_zksync_era_sha
  gl_assert_contracts_sha
}

gl_prepare_zksync_era_source_repo() {
  gl_require PROTOCOL_VERSION
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  local source_root source_path
  source_root="${ZKSYNC_ERA_SOURCE_ROOT:-${HOME}/.cache/zksync-gateway-era-source}"
  source_path="${source_root}/${PROTOCOL_VERSION}/${REQUIRED_ZKSTACK_CLI_SHA}"
  export ZKSYNC_ERA_SOURCE_PATH="${source_path}"
  echo "gateway-launch: ensuring clean zksync-era source at ${ZKSYNC_ERA_SOURCE_PATH}"

  local saved_path="${ZKSYNC_ERA_PATH:-}"
  export ZKSYNC_ERA_PATH="${ZKSYNC_ERA_SOURCE_PATH}"
  gl_prepare_zksync_era_repo
  local source_top_level_delta
  source_top_level_delta="$(git -C "${ZKSYNC_ERA_PATH}" diff --name-only -- . ":(exclude)contracts")"
  if [ -n "${source_top_level_delta}" ] || [ -n "$(git -C "${ZKSYNC_ERA_PATH}/contracts" status --porcelain)" ]; then
    gl_die "zksync-era source cache is dirty: ${ZKSYNC_ERA_PATH}"
  fi
  export ZKSYNC_ERA_PATH="${saved_path}"
}

gl_workspace_matches_required_pins() {
  gl_require ZKSYNC_ERA_PATH
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    return 1
  fi

  local top_head contracts_head
  top_head="$(git -C "${ZKSYNC_ERA_PATH}" rev-parse HEAD 2>/dev/null || true)"
  contracts_head="$(git -C "${ZKSYNC_ERA_PATH}/contracts" rev-parse HEAD 2>/dev/null || true)"
  [ "${top_head}" = "${REQUIRED_ZKSTACK_CLI_SHA}" ] && [ "${contracts_head}" = "${REQUIRED_CONTRACTS_SHA}" ]
}

# Clone zksync-era if needed, pin top + contracts to versions.yaml, build zkstack if missing.
# If ZKSYNC_ERA_PATH is unset, uses a clean shared source cache and a per-ecosystem mutable working clone.
gl_ensure_zksync_era_workspace() {
  gl_require ZKSYNC_OS_SERVER_PATH
  gl_require PROTOCOL_VERSION
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  if [ -n "${ZKSYNC_ERA_PATH:-}" ]; then
    gl_prepare_zksync_era_repo
    return 0
  fi

  gl_prepare_zksync_era_source_repo

  local run_root run_name
  run_root="${ZKSYNC_ERA_RUN_ROOT:-$(dirname "${GATEWAY_DIR:-${HOME}/gateway}")/.zksync-era-workspaces}"
  run_name="${GATEWAY_ECOSYSTEM_NAME:-$(basename "${GATEWAY_DIR:-gateway}")}-${PROTOCOL_VERSION}-${REQUIRED_ZKSTACK_CLI_SHA}"
  export ZKSYNC_ERA_PATH="${run_root}/${run_name}"
  echo "gateway-launch: ZKSYNC_ERA_PATH unset — using mutable workspace ${ZKSYNC_ERA_PATH}"

  if [ ! -d "${ZKSYNC_ERA_PATH}/.git" ]; then
    mkdir -p "${run_root}"
    git clone "${ZKSYNC_ERA_SOURCE_PATH}" "${ZKSYNC_ERA_PATH}"
    git -C "${ZKSYNC_ERA_PATH}" submodule update --init --recursive
  fi

  if gl_workspace_matches_required_pins; then
    return 0
  fi

  gl_prepare_zksync_era_repo
}

gl_path_for_zkstack() {
  gl_require ZKSYNC_ERA_PATH
  export PATH="${ZKSYNC_ERA_PATH}/zkstack_cli/target/release:${HOME}/.foundry/bin:${HOME}/.cargo/bin:${PATH}"
}

# zkstack ecosystem create writes the workspace under GATEWAY_ECOSYSTEM_PARENT_DIR using a filesystem-safe
# directory name (observed: '-' in --ecosystem-name becomes '_' on disk). Subshell ecosystem-create does not
# export back to run-gateway-launch, so we re-resolve GATEWAY_DIR here.
gl_resolve_gateway_dir_after_ecosystem_create() {
  gl_require GATEWAY_DIR
  local parent eco cand norm

  parent="${GATEWAY_ECOSYSTEM_PARENT_DIR:-$(dirname "${GATEWAY_DIR}")}"
  parent="$(cd "${parent}" && pwd)"
  eco="${GATEWAY_ECOSYSTEM_NAME:-$(basename "${GATEWAY_DIR}")}"

  if [ -f "${GATEWAY_DIR}/ZkStack.yaml" ]; then
    GATEWAY_DIR="$(cd "$(dirname "${GATEWAY_DIR}")" && pwd)/$(basename "${GATEWAY_DIR}")"
    export GATEWAY_DIR
    return 0
  fi

  cand="${parent}/${eco}"
  if [ -f "${cand}/ZkStack.yaml" ]; then
    export GATEWAY_DIR="${cand}"
    echo "gateway-launch: ecosystem directory ${GATEWAY_DIR}"
    return 0
  fi

  norm="${eco//-/_}"
  cand="${parent}/${norm}"
  if [ -f "${cand}/ZkStack.yaml" ]; then
    export GATEWAY_DIR="${cand}"
    echo "gateway-launch: ecosystem directory ${GATEWAY_DIR} (zkstack normalized '${eco}' -> '${norm}')"
    return 0
  fi

  gl_die "after ecosystem create: no ZkStack.yaml under ${parent}/${eco} or ${parent}/${norm} (set GATEWAY_DIR or GATEWAY_ECOSYSTEM_PARENT_DIR explicitly)"
}

# run-gateway-launch uses `exec > >(tee log)`: stdout is a pipe, not a TTY. zkstack/cliclack then
# panics (select.rs NotConnected). util-linux `script` runs the command with a real PTY slave.
gl_zkstack_pty() {
  if [[ "$(uname -s)" == "Linux" ]]; then
    script -q -c "$(printf '%q ' "$@")" /dev/null
  else
    "$@"
  fi
}

gl_ensure_chain_contracts_yaml_schema() {
  gl_require GATEWAY_DIR
  local chain_name contracts_yaml gateway_chain_name gateway_contracts_yaml
  chain_name="${1:?chain name required}"
  gateway_chain_name="${GATEWAY_CHAIN_NAME:-gateway}"
  contracts_yaml="${GATEWAY_DIR}/chains/${chain_name}/configs/contracts.yaml"
  [ -f "${contracts_yaml}" ] || gl_die "missing contracts config: ${contracts_yaml}"
  gateway_contracts_yaml="${GATEWAY_DIR}/chains/${gateway_chain_name}/configs/contracts.yaml"

  python3 - "${contracts_yaml}" "${chain_name}" "${gateway_chain_name}" "${gateway_contracts_yaml}" <<'PY'
import sys
import re
from pathlib import Path

import yaml

contracts_path = Path(sys.argv[1])
chain_name = sys.argv[2]
gateway_chain_name = sys.argv[3]
gateway_contracts_path = Path(sys.argv[4])
data = yaml.safe_load(contracts_path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {contracts_path}")

gateway_data = None
if chain_name != gateway_chain_name and gateway_contracts_path.exists():
    gateway_data = yaml.safe_load(gateway_contracts_path.read_text(encoding="utf-8"))
    if not isinstance(gateway_data, dict):
        gateway_data = None

updated = False

l2 = data.get("l2")
if l2 is None:
    l2 = {}
    data["l2"] = l2
    updated = True
if not isinstance(l2, dict):
    raise SystemExit(f"invalid l2 section in {contracts_path}")

if "default_l2_upgrader" not in l2:
    # Backward-compatible default for older generated contracts.yaml files.
    l2["default_l2_upgrader"] = "0x0000000000000000000000000000000000000000"
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        "(added l2.default_l2_upgrader=0x0000000000000000000000000000000000000000)"
    )

if "testnet_paymaster_addr" not in l2:
    # Optional deployment in this flow; keep schema-compliant sentinel when not deployed.
    l2["testnet_paymaster_addr"] = "0x0000000000000000000000000000000000000000"
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        "(added l2.testnet_paymaster_addr=0x0000000000000000000000000000000000000000)"
    )

eco = data.get("ecosystem_contracts")
if eco is None:
    eco = {}
    data["ecosystem_contracts"] = eco
    updated = True
if not isinstance(eco, dict):
    raise SystemExit(f"invalid ecosystem_contracts section in {contracts_path}")

l1 = data.get("l1")
if l1 is not None and not isinstance(l1, dict):
    l1 = None

gateway_eco = None
if isinstance(gateway_data, dict):
    candidate = gateway_data.get("ecosystem_contracts")
    if isinstance(candidate, dict):
        gateway_eco = candidate

def maybe_get(mapping, key):
    if isinstance(mapping, dict):
        value = mapping.get(key)
        if value is not None:
            return value
    return None

def normalize_scalar(value):
    # YAML can parse 0x-prefixed scalars as Python ints; convert back to hex
    # to avoid huge decimal string conversion failures when dumping.
    if isinstance(value, int):
        return hex(value)
    return value

def _parse_hex_like(value):
    if isinstance(value, int):
        return value
    if not isinstance(value, str):
        return None
    s = value.strip()
    if s == "":
        return None
    if s.startswith(("0x", "0X")):
        body = s[2:]
        if body == "" or not re.fullmatch(r"[0-9a-fA-F]+", body):
            return None
        return int(body, 16)
    if re.fullmatch(r"[0-9a-fA-F]+", s):
        return int(s, 16)
    return None

def normalize_address(value):
    parsed = _parse_hex_like(value)
    if parsed is None:
        return normalize_scalar(value)
    return "0x" + format(parsed & ((1 << 160) - 1), "040x")

def normalize_bytes_hex(value):
    parsed = _parse_hex_like(value)
    if parsed is None:
        return normalize_scalar(value)
    body = format(parsed, "x")
    if len(body) % 2 == 1:
        body = "0" + body
    return "0x" + body

def is_zero_like_address(value):
    value = normalize_scalar(value)
    if isinstance(value, int):
        return value == 0
    if not isinstance(value, str):
        return False
    s = value.strip().lower()
    if s in {"0x0", "0x", "0"}:
        return True
    if s.startswith("0x"):
        body = s[2:]
        return body != "" and set(body) == {"0"}
    return False

def pick_value(*candidates, prefer_non_zero=False):
    normalized = [normalize_scalar(v) for v in candidates if v is not None]
    if not normalized:
        return None
    if not prefer_non_zero:
        return normalized[0]
    for v in normalized:
        if not is_zero_like_address(v):
            return v
    return normalized[0]

# Required core ecosystem fields in current schema.
required_eco_core_fields = (
    "bridgehub_proxy_addr",
    "transparent_proxy_admin_addr",
)
for eco_key in required_eco_core_fields:
    if eco.get(eco_key) is not None:
        continue
    value = maybe_get(gateway_eco, eco_key)
    if value is None:
        raise SystemExit(
            f"unable to auto-heal required ecosystem_contracts field in {contracts_path}: {eco_key}"
        )
    eco[eco_key] = value
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        f"(added ecosystem_contracts.{eco_key}={value})"
    )

# Required CTM fields in ecosystem_contracts for current zkstack parser.
required_eco_fields = {
    "governance": ("governance_addr",),
    "chain_admin": ("chain_admin_addr",),
    "proxy_admin": ("chain_proxy_admin_addr",),
    "state_transition_proxy_addr": (),
    "validator_timelock_addr": ("validator_timelock_addr",),
    "diamond_cut_data": (),
    "l1_bytecodes_supplier_addr": (),
    "server_notifier_proxy_addr": (),
    "default_upgrade_addr": ("default_upgrade_addr",),
    "genesis_upgrade_addr": (),
    "verifier_addr": ("verifier_addr",),
    "rollup_l1_da_validator_addr": ("rollup_l1_da_validator_addr",),
    "no_da_validium_l1_validator_addr": ("no_da_validium_l1_validator_addr",),
    "avail_l1_da_validator_addr": ("avail_l1_da_validator_addr",),
    "l1_rollup_da_manager": (),
}

unresolved = []
for eco_key, l1_keys in required_eco_fields.items():
    if eco.get(eco_key) is not None:
        continue

    l1_value = None
    for l1_key in l1_keys:
        l1_value = maybe_get(l1, l1_key)
        if l1_value is not None:
            break
    gw_value = maybe_get(gateway_eco, eco_key)
    transparent_proxy_admin_value = maybe_get(eco, "transparent_proxy_admin_addr") if eco_key == "proxy_admin" else None

    # For address-like CTM fields, avoid keeping zero placeholders from stale edge
    # configs when canonical ecosystem values are available.
    value = pick_value(
        l1_value,
        gw_value,
        transparent_proxy_admin_value,
        prefer_non_zero=True,
    )

    if value is None:
        unresolved.append(eco_key)
        continue

    if eco_key in {
        "governance",
        "chain_admin",
        "proxy_admin",
        "state_transition_proxy_addr",
        "validator_timelock_addr",
        "l1_bytecodes_supplier_addr",
        "server_notifier_proxy_addr",
        "default_upgrade_addr",
        "genesis_upgrade_addr",
        "verifier_addr",
        "rollup_l1_da_validator_addr",
        "no_da_validium_l1_validator_addr",
        "avail_l1_da_validator_addr",
        "l1_rollup_da_manager",
    }:
        value = normalize_address(value)
    elif eco_key == "diamond_cut_data":
        value = normalize_bytes_hex(value)

    eco[eco_key] = value
    updated = True
    print(
        f"gateway-launch: patched {contracts_path} for {chain_name} "
        f"(added ecosystem_contracts.{eco_key}={value})"
    )

if unresolved:
    unresolved_csv = ", ".join(unresolved)
    raise SystemExit(
        f"unable to auto-heal required ecosystem_contracts fields in {contracts_path}: {unresolved_csv}"
    )

if updated:
    address_key_hints = {
        "governance",
        "chain_admin",
        "proxy_admin",
        "l1_rollup_da_manager",
    }

    def normalize_tree(obj, key_hint=None):
        if isinstance(obj, dict):
            return {k: normalize_tree(v, k) for k, v in obj.items()}
        if isinstance(obj, list):
            return [normalize_tree(v, key_hint) for v in obj]

        if isinstance(key_hint, str):
            if key_hint in address_key_hints or key_hint.endswith("_addr") or key_hint.endswith("_address"):
                return normalize_address(obj)
            if key_hint in {"diamond_cut_data", "force_deployments_data"}:
                return normalize_bytes_hex(obj)
        return normalize_scalar(obj)

    data = normalize_tree(data)
    contracts_path.write_text(
        yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )
PY
}

gl_fund_wallets_yaml() {
  gl_require GATEWAY_DIR
  gl_require L1_RPC_URL
  gl_require WALLETS_YAML_PATH
  [ -f "${WALLETS_YAML_PATH}" ] || gl_die "missing wallets file ${WALLETS_YAML_PATH}"
  export FUNDER_PRIVATE_KEY="${FUNDER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
  export WALLETS_YAML_PATH
  python3 - <<'PY'
import os
import subprocess
import time

import yaml
from pathlib import Path

w = yaml.safe_load(Path(os.environ["WALLETS_YAML_PATH"]).read_text())
rpc = os.environ["L1_RPC_URL"]
pk = os.environ["FUNDER_PRIVATE_KEY"]
l1_network = os.environ.get("L1_NETWORK", "").lower()


def addr_hex(a):
    if isinstance(a, int):
        return "0x" + format(a & ((1 << 160) - 1), "040x")
    s = str(a).strip()
    return s if s.startswith("0x") else "0x" + s


def wei_balance(address):
    return int(
        subprocess.check_output(
            ["cast", "balance", address, "--block", "pending", "--rpc-url", rpc],
            text=True,
        ).strip()
    )


def wei_balance_latest(address):
    return int(
        subprocess.check_output(
            ["cast", "balance", address, "--rpc-url", rpc],
            text=True,
        ).strip()
    )


def required_balance(role):
    if role == "deployer":
        return int(6 * 10**18)
    if role == "governor":
        return int(11 * 10**18)
    return int(10**18)


default_send_timeout = "900" if l1_network in {"tanenbaum", "mainnet"} else "45"
default_rpc_timeout = "120" if l1_network in {"tanenbaum", "mainnet"} else "45"
default_min_topup_wei = str(25 * 10**16) if l1_network in {"tanenbaum", "mainnet"} else "0"
default_post_fund_wait_timeout = "2400" if l1_network in {"tanenbaum", "mainnet"} else "120"
default_post_fund_poll_interval = "5"
send_timeout = os.environ.get("GATEWAY_FUND_TX_TIMEOUT", default_send_timeout)
rpc_timeout = os.environ.get("GATEWAY_FUND_RPC_TIMEOUT", default_rpc_timeout)
min_topup_wei = int(os.environ.get("GATEWAY_FUND_MIN_TOPUP_WEI", default_min_topup_wei))
post_fund_wait_timeout = int(
    os.environ.get("GATEWAY_FUND_POST_WAIT_TIMEOUT", default_post_fund_wait_timeout)
)
post_fund_poll_interval = float(
    os.environ.get("GATEWAY_FUND_POST_WAIT_POLL_INTERVAL", default_post_fund_poll_interval)
)


funder = subprocess.check_output(
    ["cast", "wallet", "address", "--private-key", pk],
    text=True,
).strip()
funder_balance = wei_balance(funder)
starting_nonce = int(
    subprocess.check_output(
        ["cast", "nonce", funder, "--block", "pending", "--rpc-url", rpc],
        text=True,
    ).strip()
)

transfers = []
wait_only = []
for role, cfg in w.items():
    if role == "test_wallet":
        continue
    address = addr_hex(cfg["address"])
    target = required_balance(role)
    current_latest = wei_balance_latest(address)
    current_pending = wei_balance(address)
    deficit = max(0, target - current_latest)
    if deficit == 0:
        print(
            f"wallet {role} already funded on latest: current_latest={current_latest} "
            f"current_pending={current_pending} target={target}"
        )
        continue
    if current_pending >= target:
        print(
            f"wallet {role} has pending top-up in-flight: current_latest={current_latest} "
            f"current_pending={current_pending} target={target}; waiting for confirmation"
        )
        wait_only.append((role, address, current_latest, target))
        continue
    if deficit < min_topup_wei:
        print(
            f"wallet {role} below target by dust: current_latest={current_latest} "
            f"current_pending={current_pending} target={target} "
            f"deficit={deficit} min_topup_wei={min_topup_wei}; skipping top-up"
        )
        continue
    transfers.append((role, address, current_latest, target, deficit))

if not transfers and not wait_only:
    print("all wallets already meet required balances on latest; skipping funding")
    raise SystemExit(0)

total_deficit = sum(deficit for _, _, _, _, deficit in transfers)
if funder_balance < total_deficit:
    raise SystemExit(
        f"funder {funder} has insufficient balance: balance={funder_balance} total_required={total_deficit}"
    )

for index, (role, address, current, target, deficit) in enumerate(transfers):
    nonce = starting_nonce + index
    result = subprocess.check_output(
        [
            "cast",
            "send",
            address,
            "--value",
            str(deficit),
            "--rpc-url",
            rpc,
            "--rpc-timeout",
            rpc_timeout,
            "--timeout",
            send_timeout,
            "--private-key",
            pk,
            "--nonce",
            str(nonce),
            "--async",
        ],
        text=True,
    ).strip()
    print(
        f"funding wallet {role}: current={current} target={target} deficit={deficit} "
        f"nonce={nonce} tx={result}"
    )

deadline = time.time() + post_fund_wait_timeout
pending_targets = {role: (address, target) for role, address, _, target in wait_only}
for role, address, _, target, _ in transfers:
    pending_targets[role] = (address, target)
print(
    f"waiting for funding transactions to be reflected on latest block for {len(pending_targets)} wallet(s) "
    f"(timeout={post_fund_wait_timeout}s)"
)

while pending_targets:
    completed_roles = []
    for role, (address, target) in pending_targets.items():
        current_latest = wei_balance_latest(address)
        if current_latest >= target:
            print(f"wallet {role} funded on latest: current={current_latest} target={target}")
            completed_roles.append(role)
    for role in completed_roles:
        pending_targets.pop(role, None)
    if not pending_targets:
        break
    if time.time() > deadline:
        missing = ", ".join(sorted(pending_targets.keys()))
        raise SystemExit(
            f"timed out waiting for wallet funding confirmations on latest block; still below target: {missing}"
        )
    time.sleep(post_fund_poll_interval)
PY
}
