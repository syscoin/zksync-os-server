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

if has_text "Tanenbaum" "${ERA_PATH}/core/lib/basic_types/src/network.rs" \
  && has_text "Self::Mainnet => SLChainId(57)" "${ERA_PATH}/core/lib/basic_types/src/network.rs" \
  && has_text "Self::Tanenbaum => SLChainId(5700)" "${ERA_PATH}/core/lib/basic_types/src/network.rs" \
  && has_text "Tanenbaum" "${ERA_PATH}/zkstack_cli/crates/types/src/l1_network.rs" \
  && has_text "L1Network::Mainnet => 57" "${ERA_PATH}/zkstack_cli/crates/types/src/l1_network.rs" \
  && has_text "L1Network::Tanenbaum => None" "${ERA_PATH}/zkstack_cli/crates/types/src/l1_network.rs" \
  && has_text "L1Network::Tanenbaum | L1Network::Holesky => H256::zero()" "${ERA_PATH}/zkstack_cli/crates/types/src/l1_network.rs" \
  && has_text "L1Network::Tanenbaum" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/ecosystem/init.rs" \
  && has_text "if config.l1_network == L1Network::Localhost" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/ecosystem/common.rs" \
  && has_text "forge = forge.with_slow();" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/ctm/commands/init_new_ctm.rs" \
  && has_text "if config.l1_network == L1Network::Localhost" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/ecosystem/register_ctm.rs" \
  && has_text "if chain_config.l1_network == L1Network::Localhost" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/chain/register_chain.rs" \
  && has_text "if chain_config.l1_network == L1Network::Localhost" "${ERA_PATH}/zkstack_cli/crates/zkstack/src/commands/chain/deploy_l2_contracts.rs"; then
  echo "zksync-era Syscoin patch appears already applied; skipping."
  exit 0
fi

echo "Checking patch applicability..."
git -C "${ERA_PATH}" apply --check --recount "${PATCH_FILE}"

echo "Applying Syscoin/Tanenbaum compatibility patch..."
git -C "${ERA_PATH}" apply --recount "${PATCH_FILE}"

echo "Patch applied successfully."
