#!/usr/bin/env bash
# Fund addresses in wallets.yaml on L1_RPC_URL.
# zkstack --zksync-os may only create chains/<name>/configs/wallets.yaml (no top-level configs/).
# To avoid partial funding when wallet files diverge (e.g. root vs chain-scoped configs),
# fund all discovered wallet files (deduped) plus optional explicit paths.
# Default funder: Anvil dev key 0. Override: FUNDER_PRIVATE_KEY
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
gl_require GATEWAY_DIR
gl_require L1_RPC_URL
: "${GATEWAY_CHAIN_NAME:=gateway}"

ROOT_W="${GATEWAY_DIR}/configs/wallets.yaml"
CHAIN_W="${GATEWAY_DIR}/chains/${GATEWAY_CHAIN_NAME}/configs/wallets.yaml"

normalize_path() {
  python3 - "$1" <<'PY'
import os, sys
print(os.path.realpath(sys.argv[1]))
PY
}

validate_wallet_path_in_gateway_dir() {
  local p="$1"
  python3 - "${GATEWAY_DIR}" "${p}" <<'PY'
import sys
from pathlib import Path

gateway_dir = Path(sys.argv[1]).resolve(strict=True)
wallet_path = Path(sys.argv[2]).resolve(strict=True)
try:
    wallet_path.relative_to(gateway_dir)
except ValueError:
    raise SystemExit(
        f"wallet file must be inside GATEWAY_DIR ({gateway_dir}): {wallet_path}"
    )
PY
}

declare -a wallet_files=()
declare -a wallet_files_norm=()

add_wallet_file() {
  local p="$1" norm existing
  [ -f "${p}" ] || return 0
  gl_prepare_wallet_file_for_in_file "${p}"
  validate_wallet_path_in_gateway_dir "${p}"
  norm="$(normalize_path "${p}")"
  if [ "${#wallet_files_norm[@]}" -gt 0 ]; then
    for existing in "${wallet_files_norm[@]}"; do
      if [ "${existing}" = "${norm}" ]; then
        return 0
      fi
    done
  fi
  wallet_files+=("${p}")
  wallet_files_norm+=("${norm}")
}

# Optional explicit paths (colon-separated), checked first.
if [ -n "${GATEWAY_FUND_WALLETS_PATHS:-}" ]; then
  IFS=':' read -r -a explicit_wallet_paths <<<"${GATEWAY_FUND_WALLETS_PATHS}"
  for wallet_path in "${explicit_wallet_paths[@]}"; do
    [ -n "${wallet_path}" ] || continue
    add_wallet_file "${wallet_path}"
  done
fi

# Always consider both files; they may differ depending on chain create/init path.
add_wallet_file "${ROOT_W}"
add_wallet_file "${CHAIN_W}"

if [ "${#wallet_files[@]}" -eq 0 ]; then
  gl_die "no wallets.yaml found (tried ${ROOT_W} and ${CHAIN_W}; GATEWAY_FUND_WALLETS_PATHS='${GATEWAY_FUND_WALLETS_PATHS:-}')"
fi

wallet_files_joined=""
for wf in "${wallet_files[@]}"; do
  echo "gateway-launch: funding wallets from ${wf}"
  if [ -z "${wallet_files_joined}" ]; then
    wallet_files_joined="${wf}"
  else
    wallet_files_joined="${wallet_files_joined}:${wf}"
  fi
done

WALLETS_YAML_PATHS="${wallet_files_joined}" gl_fund_wallets_yaml
