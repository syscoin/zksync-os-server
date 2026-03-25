# shellcheck shell=bash
# Source-only: shared paths and helpers for gateway-launch/*.sh
# shellcheck disable=SC2034
GL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZKSYNC_OS_SERVER_PATH="${ZKSYNC_OS_SERVER_PATH:-$(cd "${GL_DIR}/../.." && pwd)}"

gl_die() {
  echo "gateway-launch: $*" >&2
  exit 1
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

# Clone zksync-era if needed, pin top + contracts to versions.yaml, build zkstack if missing.
# If ZKSYNC_ERA_PATH is unset, uses ZKSYNC_ERA_CACHE_ROOT/PROTOCOL_VERSION/ZKSTACK_CLI_SHA (default cache ~/.cache/zksync-gateway-era).
gl_ensure_zksync_era_workspace() {
  gl_require ZKSYNC_OS_SERVER_PATH
  gl_require PROTOCOL_VERSION
  gl_require REQUIRED_ZKSTACK_CLI_SHA
  gl_require REQUIRED_CONTRACTS_SHA

  local url="${ZKSYNC_ERA_GIT_URL:-https://github.com/matter-labs/zksync-era.git}"

  if [ -z "${ZKSYNC_ERA_PATH:-}" ]; then
    export ZKSYNC_ERA_PATH="${ZKSYNC_ERA_CACHE_ROOT:-${HOME}/.cache/zksync-gateway-era}/${PROTOCOL_VERSION}/${REQUIRED_ZKSTACK_CLI_SHA}"
    echo "gateway-launch: ZKSYNC_ERA_PATH unset — using ${ZKSYNC_ERA_PATH}"
  fi

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

gl_fund_wallets_yaml() {
  gl_require GATEWAY_DIR
  gl_require L1_RPC_URL
  gl_require WALLETS_YAML_PATH
  [ -f "${WALLETS_YAML_PATH}" ] || gl_die "missing wallets file ${WALLETS_YAML_PATH}"
  export FUNDER_PRIVATE_KEY="${FUNDER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
  export WALLETS_YAML_PATH
  python3 - <<'PY'
import json
import os
import subprocess
import urllib.request

import yaml
from pathlib import Path

w = yaml.safe_load(Path(os.environ["WALLETS_YAML_PATH"]).read_text())
rpc = os.environ["L1_RPC_URL"]
pk = os.environ["FUNDER_PRIVATE_KEY"]


def addr_hex(a):
    if isinstance(a, int):
        return "0x" + format(a & ((1 << 160) - 1), "040x")
    s = str(a).strip()
    return s if s.startswith("0x") else "0x" + s


def rpc_call(method, params):
    req = urllib.request.Request(
        rpc,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=45) as resp:
        payload = json.loads(resp.read().decode())
    if payload.get("error") is not None:
        raise SystemExit(f"rpc error for {method}: {payload['error']}")
    return payload["result"]


def wei_balance(address):
    return int(rpc_call("eth_getBalance", [address, "pending"]), 16)


def required_balance(role):
    if role == "deployer":
        return int(6 * 10**18)
    if role == "governor":
        return int(11 * 10**18)
    return int(10**18)


funder = subprocess.check_output(
    ["cast", "wallet", "address", "--private-key", pk],
    text=True,
).strip()
funder_balance = wei_balance(funder)
starting_nonce = int(rpc_call("eth_getTransactionCount", [funder, "pending"]), 16)

transfers = []
for role, cfg in w.items():
    if role == "test_wallet":
        continue
    address = addr_hex(cfg["address"])
    target = required_balance(role)
    current = wei_balance(address)
    deficit = max(0, target - current)
    if deficit == 0:
        print(f"wallet {role} already funded: current={current} target={target}")
        continue
    transfers.append((role, address, current, target, deficit))

if not transfers:
    print("all wallets already meet required balances; skipping funding")
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
PY
}
