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

gl_path_for_zkstack() {
  gl_require ZKSYNC_ERA_PATH
  export PATH="${ZKSYNC_ERA_PATH}/zkstack_cli/target/release:${HOME}/.foundry/bin:${HOME}/.cargo/bin:${PATH}"
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
  export FUNDER_PRIVATE_KEY="${FUNDER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
  export WALLETS_YAML_PATH="${WALLETS_YAML_PATH:-${GATEWAY_DIR}/configs/wallets.yaml}"
  python3 - <<'PY'
import os, subprocess, yaml
from pathlib import Path

w = yaml.safe_load(Path(os.environ["WALLETS_YAML_PATH"]).read_text())
rpc = os.environ["L1_RPC_URL"]
pk = os.environ["FUNDER_PRIVATE_KEY"]


def addr_hex(a):
    if isinstance(a, int):
        return "0x" + format(a & ((1 << 160) - 1), "040x")
    s = str(a).strip()
    return s if s.startswith("0x") else "0x" + s


for role, wei in [("deployer", int(6 * 10**18)), ("governor", int(6 * 10**18))]:
    subprocess.run(
        ["cast", "send", addr_hex(w[role]["address"]), "--value", str(wei), "--rpc-url", rpc, "--private-key", pk],
        check=True,
    )
for role in w:
    if role in ("deployer", "governor", "test_wallet"):
        continue
    subprocess.run(
        ["cast", "send", addr_hex(w[role]["address"]), "--value", str(10**18), "--rpc-url", rpc, "--private-key", pk],
        check=True,
    )
PY
}
