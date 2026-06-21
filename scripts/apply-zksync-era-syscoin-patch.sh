#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/zksync-era" >&2
  exit 1
fi

ERA_PATH="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/zksync-era-syscoin.patch"

if [[ ! -d "${ERA_PATH}/.git" ]]; then
  echo "error: ${ERA_PATH} is not a git repository root" >&2
  exit 1
fi

if [[ ! -f "${PATCH_FILE}" ]]; then
  echo "error: patch file not found: ${PATCH_FILE}" >&2
  exit 1
fi

# Idempotency guard: if all marker changes are already present, skip apply.
# Prefer rg when available (faster), otherwise fallback to grep.
has_text() {
  local needle="$1"
  local file="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q --fixed-strings "$needle" "$file"
  else
    grep -q --fixed-strings "$needle" "$file"
  fi
}

missing_patch_paths=()
add_missing_patch_path() {
  local path="$1"
  local existing
  for existing in "${missing_patch_paths[@]}"; do
    if [[ "${existing}" == "${path}" ]]; then
      return 0
    fi
  done
  missing_patch_paths+=("${path}")
}

require_marker() {
  local needle="$1"
  local path="$2"
  if ! has_text "${needle}" "${ERA_PATH}/${path}"; then
    add_missing_patch_path "${path}"
  fi
}

require_marker "Tanenbaum" "core/lib/basic_types/src/network.rs"
require_marker "Self::Mainnet => SLChainId(57)" "core/lib/basic_types/src/network.rs"
require_marker "Self::Tanenbaum => SLChainId(5700)" "core/lib/basic_types/src/network.rs"
require_marker "Tanenbaum" "zkstack_cli/crates/types/src/l1_network.rs"
require_marker "L1Network::Mainnet => 57" "zkstack_cli/crates/types/src/l1_network.rs"
require_marker "L1Network::Tanenbaum => None" "zkstack_cli/crates/types/src/l1_network.rs"
require_marker "L1Network::Tanenbaum | L1Network::Holesky => H256::zero()" "zkstack_cli/crates/types/src/l1_network.rs"
require_marker "L1Network::Tanenbaum" "zkstack_cli/crates/zkstack/src/commands/ecosystem/init.rs"
require_marker "if config.l1_network == L1Network::Localhost" "zkstack_cli/crates/zkstack/src/commands/ecosystem/common.rs"
require_marker "forge = forge.with_slow();" "zkstack_cli/crates/zkstack/src/commands/ctm/commands/init_new_ctm.rs"
require_marker "if config.l1_network == L1Network::Localhost" "zkstack_cli/crates/zkstack/src/commands/ecosystem/register_ctm.rs"
require_marker "if chain_config.l1_network == L1Network::Localhost" "zkstack_cli/crates/zkstack/src/commands/chain/register_chain.rs"
require_marker "if chain_config.l1_network == L1Network::Localhost" "zkstack_cli/crates/zkstack/src/commands/chain/deploy_l2_contracts.rs"
require_marker "min_validator_balance: U256::from(10).pow(18.into())," "zkstack_cli/crates/zkstack/src/commands/chain/gateway/migrate_to_gateway.rs"

if [[ "${#missing_patch_paths[@]}" -eq 0 ]]; then
  echo "zksync-era Syscoin patch appears already applied; skipping."
  exit 0
fi

echo "Checking patch applicability..."
for path in "${missing_patch_paths[@]}"; do
  git -C "${ERA_PATH}" apply --check --recount --include="${path}" "${PATCH_FILE}"
done

echo "Applying Syscoin/Tanenbaum compatibility patch..."
for path in "${missing_patch_paths[@]}"; do
  git -C "${ERA_PATH}" apply --recount --include="${path}" "${PATCH_FILE}"
done

echo "Patch applied successfully."
