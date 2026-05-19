#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"

usage() {
  cat <<'EOF' >&2
Usage: run-os-server-with-patched-zksync-os.sh <workspace-name> -- <cargo args...>
Example:
  run-os-server-with-patched-zksync-os.sh gateway -- run --release -- --config /path/to/config.yaml
EOF
  exit 1
}

[ $# -ge 3 ] || usage
WORKSPACE_NAME="$1"
shift
[ "${1:-}" = "--" ] || usage
shift
[ $# -gt 0 ] || usage

gl_require GATEWAY_DIR
gl_require ZKSYNC_OS_SERVER_PATH
: "${PROTOCOL_VERSION:=v31.0}"
: "${ZKSYNC_OS_GIT_URL:=https://github.com/matter-labs/zksync-os.git}"

protocol_uses_dev_patch() {
  case "${PROTOCOL_VERSION}" in
  v31.* | v32.*) return 0 ;;
  *) return 1 ;;
  esac
}

extract_dev_tag() {
  python3 - "${ZKSYNC_OS_SERVER_PATH}/Cargo.toml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
m = re.search(
    r'zk_os_forward_system_dev\s*=\s*\{\s*package\s*=\s*"forward_system",\s*git\s*=\s*"[^"]+",\s*tag\s*=\s*"([^"]+)"',
    text,
    re.MULTILINE,
)
if not m:
    raise SystemExit("failed to locate zk_os_forward_system_dev tag in Cargo.toml")
print(m.group(1))
PY
}

extract_dev_git_url() {
  python3 - "${ZKSYNC_OS_SERVER_PATH}/Cargo.toml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
m = re.search(
    r'zk_os_forward_system_dev\s*=\s*\{\s*package\s*=\s*"forward_system",\s*git\s*=\s*"([^"]+)"',
    text,
    re.MULTILINE,
)
if not m:
    raise SystemExit("failed to locate zk_os_forward_system_dev git URL in Cargo.toml")
print(m.group(1))
PY
}

extract_locked_rev() {
  python3 - "${ZKSYNC_OS_SERVER_PATH}/Cargo.lock" "$1" "$2" <<'PY'
import re
import sys
from pathlib import Path

def normalize_git_url(url: str) -> str:
    return url.removesuffix(".git")

lock_text = Path(sys.argv[1]).read_text(encoding="utf-8")
dev_git_url = normalize_git_url(sys.argv[2])
dev_tag = sys.argv[3]

for m in re.finditer(
    r'source\s*=\s*"git\+([^"?]+)\?tag=([^"#]+)#([0-9a-f]{40})"',
    lock_text,
):
    source_url, source_tag, locked_rev = m.groups()
    if normalize_git_url(source_url) == dev_git_url and source_tag == dev_tag:
        print(locked_rev)
        raise SystemExit(0)

raise SystemExit(
    f"failed to locate locked zksync-os revision for {sys.argv[2]} tag {dev_tag}"
)
PY
}

checkout_locked_base() {
  local dev_path="$1"
  local locked_rev="$2"

  if ! git -C "${dev_path}" cat-file -e "${locked_rev}^{commit}" 2>/dev/null; then
    git -C "${dev_path}" fetch --tags "${ZKSYNC_OS_GIT_URL}" >/dev/null || \
      gl_die "failed to fetch locked zksync-os revision ${locked_rev} from ${ZKSYNC_OS_GIT_URL}"
  fi
  git -C "${dev_path}" cat-file -e "${locked_rev}^{commit}" 2>/dev/null || \
    gl_die "locked zksync-os revision ${locked_rev} is unavailable in ${dev_path}"

  if [ -n "$(git -C "${dev_path}" status --porcelain)" ]; then
    gl_die "zksync-os checkout has local changes; cannot reset to locked revision: ${dev_path}"
  fi

  git -C "${dev_path}" checkout --detach "${locked_rev}" >/dev/null
  local checked_out_rev
  checked_out_rev="$(git -C "${dev_path}" rev-parse HEAD)"
  [ "${checked_out_rev}" = "${locked_rev}" ] || \
    gl_die "zksync-os checkout ${checked_out_rev} != locked revision ${locked_rev}"
}

prepare_dev_checkout() {
  local dev_tag dev_git_url locked_rev dev_root dev_path
  dev_tag="${1:?dev tag required}"
  dev_git_url="$(extract_dev_git_url)"
  locked_rev="$(extract_locked_rev "${dev_git_url}" "${dev_tag}")"

  if [ -n "${ZKSYNC_OS_DEV_PATH:-}" ]; then
    dev_path="${ZKSYNC_OS_DEV_PATH}"
    git -C "${dev_path}" rev-parse --show-toplevel >/dev/null 2>&1 || \
      gl_die "ZKSYNC_OS_DEV_PATH is not a git repository root: ${dev_path}"
    # SYSCOIN: the launcher rewrites dependencies to a patched local zksync-os
    # checkout, so re-anchor that checkout to Cargo.lock before applying the patch.
    checkout_locked_base "${dev_path}" "${locked_rev}"
    bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-os-syscoin-patch.sh" "${dev_path}"
    git -C "${dev_path}" add -A
    if ! git -C "${dev_path}" diff --cached --quiet; then
      git -C "${dev_path}" -c user.name="gateway-launch" -c user.email="gateway-launch@local" \
        commit -m "gateway-launch local syscoin patch" >/dev/null
    fi
    git -C "${dev_path}" tag -f "${dev_tag}" >/dev/null
    printf '%s\n' "${dev_path}"
    return 0
  fi

  dev_root="${GATEWAY_DIR}/.gateway-launch/zksync-os"
  dev_path="${dev_root}/${dev_tag}"

  if [ ! -d "${dev_path}/.git" ]; then
    mkdir -p "${dev_root}"
    git clone "${ZKSYNC_OS_GIT_URL}" "${dev_path}"
  fi

  # SYSCOIN: use Cargo.lock's immutable git revision, not the mutable upstream tag.
  checkout_locked_base "${dev_path}" "${locked_rev}"
  bash "${ZKSYNC_OS_SERVER_PATH}/scripts/apply-zksync-os-syscoin-patch.sh" "${dev_path}"
  git -C "${dev_path}" add -A
  if ! git -C "${dev_path}" diff --cached --quiet; then
    git -C "${dev_path}" -c user.name="gateway-launch" -c user.email="gateway-launch@local" \
      commit -m "gateway-launch local syscoin patch" >/dev/null
  fi
  git -C "${dev_path}" tag -f "${dev_tag}" >/dev/null
  printf '%s\n' "${dev_path}"
}

prepare_run_workspace() {
  local run_path="$1"
  local dev_path="$2"
  local dev_tag="$3"
  python3 - "${ZKSYNC_OS_SERVER_PATH}" "${run_path}" "${dev_path}" "${dev_tag}" <<'PY'
import re
import shutil
import sys
from pathlib import Path

source = Path(sys.argv[1]).resolve()
target = Path(sys.argv[2]).resolve()
dev_path = Path(sys.argv[3]).resolve()
dev_tag = sys.argv[4]
dev_git_url = dev_path.as_uri()

if target.exists():
    shutil.rmtree(target)

def ignore(_dir: str, names: list[str]) -> set[str]:
    blocked = {
        ".git",
        "target",
        ".cursor",
        ".gateway-launch",
    }
    return {name for name in names if name in blocked}

shutil.copytree(source, target, ignore=ignore)

cargo_toml = target / "Cargo.toml"
text = cargo_toml.read_text(encoding="utf-8")

forward_re = re.compile(
    r'zk_os_forward_system_dev\s*=\s*\{.*?default-features\s*=\s*false\s*\}',
    re.MULTILINE | re.DOTALL,
)
zk_ee_re = re.compile(
    r'zk_ee_dev\s*=\s*\{.*?\}',
    re.MULTILINE,
)
basic_re = re.compile(
    r'zk_os_basic_system_dev\s*=\s*\{.*?\}',
    re.MULTILINE,
)

text, count_forward = forward_re.subn(
    f'zk_os_forward_system_dev = {{ package = "forward_system", git = "{dev_git_url}", tag = "{dev_tag}", features = [\n'
    '    "production",\n'
    '    "no_print",\n'
    '], default-features = false }',
    text,
    count=1,
)
text, count_ee = zk_ee_re.subn(
    f'zk_ee_dev = {{ package = "zk_ee", git = "{dev_git_url}", tag = "{dev_tag}" }}',
    text,
    count=1,
)
text, count_basic = basic_re.subn(
    f'zk_os_basic_system_dev = {{ package = "basic_system", git = "{dev_git_url}", tag = "{dev_tag}" }}',
    text,
    count=1,
)

if count_forward != 1 or count_ee != 1 or count_basic != 1:
    raise SystemExit("failed to rewrite dev zksync-os dependencies in Cargo.toml")

cargo_toml.write_text(text, encoding="utf-8")
PY
}

clear_multivm_build_script_cache() {
  local target_dir="$1"
  # SYSCOIN: prepare_run_workspace recreates lib/multivm/apps, but Cargo may
  # reuse an old build-script output that points include_bytes! at deleted files.
  rm -rf "${target_dir}"/debug/build/zksync_os_multivm-* \
    "${target_dir}"/release/build/zksync_os_multivm-*
}

refresh_os_server_config_credentials() {
  local seen_bin_args=false expect_config=false arg config_entry config_path
  local config_paths=()

  for arg in "$@"; do
    if [ "${arg}" = "--" ]; then
      seen_bin_args=true
      expect_config=false
      continue
    fi
    [ "${seen_bin_args}" = true ] || continue

    if [ "${expect_config}" = true ]; then
      config_paths+=("${arg}")
      expect_config=false
      continue
    fi

    case "${arg}" in
    --config=*)
      config_paths+=("${arg#--config=}")
      ;;
    --config)
      expect_config=true
      ;;
    esac
  done

  [ "${#config_paths[@]}" -gt 0 ] || return 0
  # SYSCOIN: syscoind rotates cookie credentials on restart. Keep generated
  # os-server configs aligned immediately before launching the node. Mirror the
  # Rust CLI's config parsing: repeated --config flags are allowed and each value
  # may contain ':'-delimited config files loaded in order.
  for config_entry in "${config_paths[@]}"; do
    while IFS= read -r config_path; do
      [ -n "${config_path}" ] || continue
      gl_refresh_bitcoin_da_config_from_cookie "${config_path}"
    done < <(printf '%s\n' "${config_entry}" | tr ':' '\n')
  done
}

refresh_os_server_config_credentials "$@"

if protocol_uses_dev_patch; then
  DEV_TAG="$(extract_dev_tag)"
  DEV_PATH="$(prepare_dev_checkout "${DEV_TAG}")"
  RUN_PATH="${GATEWAY_DIR}/.gateway-launch/zksync-os-server/${WORKSPACE_NAME}"
  TARGET_DIR="${GATEWAY_DIR}/.gateway-launch/target/${WORKSPACE_NAME}"
  prepare_run_workspace "${RUN_PATH}" "${DEV_PATH}" "${DEV_TAG}"
  clear_multivm_build_script_cache "${TARGET_DIR}"
  cd "${RUN_PATH}"
  export CARGO_TARGET_DIR="${TARGET_DIR}"
else
  cd "${ZKSYNC_OS_SERVER_PATH}"
fi

cargo "$@"
