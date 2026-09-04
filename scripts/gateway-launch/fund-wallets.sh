#!/usr/bin/env bash
# Fund addresses in wallets.yaml on L1_RPC_URL.
# zkstack --zksync-os may only create chains/<name>/configs/wallets.yaml (no top-level configs/).
# To avoid partial funding when wallet files diverge (e.g. root vs chain-scoped configs),
# fund all discovered wallet files (deduped) plus optional explicit paths.
# Funder signer: FUNDER_SIGNER=account|keystore|ledger|trezor|aws|gcp.
# Local/dev fallback: FUNDER_SIGNER=private-key uses the Anvil dev key unless
# FUNDER_PRIVATE_KEY is set. Real networks reject raw private-key argv by default.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/_common.sh"
FUND_CHECK_ONLY=false
if [ "${1:-}" = "--check-only" ]; then
  FUND_CHECK_ONLY=true
  shift
fi
[ "$#" -eq 0 ] || gl_die "usage: fund-wallets.sh [--check-only]"
gl_require GATEWAY_DIR
gl_require L1_RPC_URL
gl_require L1_CHAIN_ID
gl_require L1_NETWORK
gl_validate_l1_network_pair
: "${PROTOCOL_VERSION:=v32.0}"
export PROTOCOL_VERSION
gl_resolve_required_source_pins
: "${GATEWAY_CHAIN_NAME:=gateway}"
: "${GATEWAY_FUND_TARGET_CHAIN_NAME:=${GATEWAY_CHAIN_NAME}}"
: "${GATEWAY_FUND_EDGE_CONTEXT:=false}"
GATEWAY_FUND_EDGE_CONTEXT="$(gl_to_lower "${GATEWAY_FUND_EDGE_CONTEXT}")"
case "${GATEWAY_FUND_EDGE_CONTEXT}" in
true | false) ;;
*) gl_die "GATEWAY_FUND_EDGE_CONTEXT must be true or false" ;;
esac
if [ "${GATEWAY_FUND_EDGE_CONTEXT}" = true ] &&
  [ "${GATEWAY_FUND_TARGET_CHAIN_NAME}" != "${EDGE_CHAIN_NAME:-zksys}" ]; then
  gl_die "edge funding target must match EDGE_CHAIN_NAME"
fi
if [ "${FUND_CHECK_ONLY}" != true ]; then
  gl_validate_l1_signer_policy
  gl_acquire_gateway_launch_lock
fi
# SYSCOIN: Authenticate the exact chain immediately before direct funding
# sends; this helper is also a standalone operator entry point.
gl_l1_broadcast_preflight
if [ "${FUND_CHECK_ONLY}" = true ]; then
  # SYSCOIN: Validation remains read-only while rejecting a different launch
  # identity from the one durably bound to this workspace.
  if [ "${GATEWAY_FUND_EDGE_CONTEXT}" = true ]; then
    gl_assert_edge_launch_context
  else
    gl_checkpoint_assert_fingerprint_matches
  fi
else
  if [ "${GATEWAY_FUND_EDGE_CONTEXT}" = true ]; then
    gl_bind_edge_launch_context
  else
    gl_bind_gateway_launch_context
  fi
fi

ROOT_W="${GATEWAY_DIR}/configs/wallets.yaml"
CHAIN_W="${GATEWAY_DIR}/chains/${GATEWAY_FUND_TARGET_CHAIN_NAME}/configs/wallets.yaml"

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
  validate_wallet_path_in_gateway_dir "${p}"
  if [ "${FUND_CHECK_ONLY}" = true ]; then
    gl_validate_secret_file "${p}" "wallet file"
  else
    gl_prepare_wallet_file_for_in_file "${p}"
  fi
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
  if [ "${FUND_CHECK_ONLY}" = true ]; then
    echo "gateway-launch: checking wallet funding from ${wf}"
  else
    echo "gateway-launch: funding wallets from ${wf}"
  fi
  if [ -z "${wallet_files_joined}" ]; then
    wallet_files_joined="${wf}"
  else
    wallet_files_joined="${wallet_files_joined}:${wf}"
  fi
done

WALLETS_YAML_PATHS="${wallet_files_joined}" \
  GATEWAY_FUND_CHECK_ONLY="${FUND_CHECK_ONLY}" \
  GATEWAY_LAUNCH_HELPER_DIR="${GL_DIR}" \
  gl_fund_wallets_yaml
