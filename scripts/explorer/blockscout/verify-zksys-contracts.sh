#!/usr/bin/env bash
# Verify deployed zkSYS L2 tokenomics contracts and the L1 registry bridge on Blockscout.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"

ZKSYS_SOLC="${ZKSYS_SOLC:-v0.8.33+commit.64118f21}"
ZKSYS_TOKEN_SOLC="${ZKSYS_TOKEN_SOLC:-v0.8.28+commit.7893614a}"
ZKSYS_ADMIN="${ZKSYS_ADMIN:-0x622a54Ea3a123127cA5fe8B98DE90E957471093A}"

ZKSYS_L2_RPC_URL="${ZKSYS_L2_RPC_URL:-https://rpc-zk.tanenbaum.io}"
ZKSYS_L2_EXPLORER_BASE="${ZKSYS_L2_EXPLORER_BASE:-https://explorer-zk.tanenbaum.io}"
ZKSYS_L2_CHAIN_ID="${ZKSYS_L2_CHAIN_ID:-57057}"

L1_RPC_URL="${L1_RPC_URL:-https://rpc.tanenbaum.io}"
L1_EXPLORER_BASE="${L1_EXPLORER_BASE:-https://explorer.tanenbaum.io}"
L1_CHAIN_ID="${L1_CHAIN_ID:-5700}"

L2_PROXY_ADMIN="${L2_PROXY_ADMIN:-0xAbb78Fba301FA86Ac40440e663CeB3f640348AC8}"
TOKEN_IMPL="${TOKEN_IMPL:-0x95c548F08137E812C95D1F705f10870c3Cf7870e}"
TOKEN_PROXY="${TOKEN_PROXY:-0x6EBb170f69D886916D9ee9E585CE39E626CbC35d}"
REGISTRY_IMPL="${REGISTRY_IMPL:-0xadB8109Dd89696edC64EFb8D6e4067eA62546Da8}"
REGISTRY_PROXY="${REGISTRY_PROXY:-0x0b8647BB8f5A25D1e2a599c7805D39Dc0D876b7B}"
WEIGHT_REGISTRY_IMPL="${WEIGHT_REGISTRY_IMPL:-0x701e1675AeA9553fCB2466A33689b5b3f865b499}"
WEIGHT_REGISTRY_PROXY="${WEIGHT_REGISTRY_PROXY:-0xB7fc270CBf9e47c1157c205aa25341cce4280f9C}"
ISSUER_IMPL="${ISSUER_IMPL:-0x7d00Ff6f013D8286Edba77Cf1c53CfBed2F6E235}"
ISSUER_PROXY="${ISSUER_PROXY:-0x9e40c2d8523A4770A702BBD26d1ddf8539B9aEf5}"
STAKING_VAULT_IMPL="${STAKING_VAULT_IMPL:-0xB4305c45D2204eFFc774a680eeA9820d22970946}"
STAKING_VAULT_PROXY="${STAKING_VAULT_PROXY:-0xC94d9C7A71037bAa1Ceb0a3ca4B0C241b8b41C6B}"

L1_BRIDGE_PROXY_ADMIN="${L1_BRIDGE_PROXY_ADMIN:-0x45ACeaB9d9D04fFf7C6e16B5e305cC69D6d39e6D}"
L1_BRIDGE_IMPL="${L1_BRIDGE_IMPL:-0x5D681Fb30174E9f717B756c1F32Ae085Db95c968}"
L1_BRIDGE_PROXY="${L1_BRIDGE_PROXY:-0xcF2FbC5A132d8CB3c30d5645A04c7E570098be49}"
L1_BRIDGEHUB="${L1_BRIDGEHUB:-}"
L1_BRIDGE_NEVM_START_BLOCK="${L1_BRIDGE_NEVM_START_BLOCK:-1317500}"
L1_BRIDGE_SENIORITY_HEIGHT1="${L1_BRIDGE_SENIORITY_HEIGHT1:-210240}"
L1_BRIDGE_SENIORITY_HEIGHT2="${L1_BRIDGE_SENIORITY_HEIGHT2:-525600}"
L1_BRIDGE_SENIORITY_LEVEL1_BPS="${L1_BRIDGE_SENIORITY_LEVEL1_BPS:-0}"
L1_BRIDGE_SENIORITY_LEVEL2_BPS="${L1_BRIDGE_SENIORITY_LEVEL2_BPS:-0}"

export FOUNDRY_BYTECODE_HASH=none
export FOUNDRY_CBOR_METADATA=false

is_verified() {
  local explorer_base="${1:?explorer base required}" address="${2:?address required}"
  curl -fsS "${explorer_base%/}/api/v2/smart-contracts/${address}" 2>/dev/null | grep -q '"is_verified":true'
}

runtime_code() {
  local rpc_url="${1:?rpc url required}" address="${2:?address required}"
  cast code "${address}" --rpc-url "${rpc_url}"
}

numeric_call() {
  local rpc_url="${1:?rpc url required}" target="${2:?target required}" signature="${3:?signature required}"
  local value
  value="$(cast call --rpc-url "${rpc_url}" "${target}" "${signature}")"
  printf '%s\n' "${value%% *}"
}

require_code() {
  local rpc_url="${1:?rpc url required}" label="${2:?label required}" address="${3:?address required}"
  [ "$(runtime_code "${rpc_url}" "${address}")" != "0x" ] || {
    echo "missing ${label} (${address}); no code deployed" >&2
    return 1
  }
}

verify_contract() {
  local rpc_url="${1:?rpc url required}"
  local explorer_base="${2:?explorer base required}"
  local chain_id="${3:?chain id required}"
  local compiler_version="${4:?compiler version required}"
  local address="${5:?address required}"
  local label="${6:?label required}"
  local contract="${7:?contract required}"
  local ctor="${8:-}"

  require_code "${rpc_url}" "${label}" "${address}"
  if is_verified "${explorer_base}" "${address}"; then
    echo "skip   ${label} (${address}) already verified"
    return
  fi

  echo "submit ${label} (${address})"
  (
    cd "${CONTRACTS_DIR}"
    args=(
      verify-contract
      "${address}"
      "${contract}"
      --chain "${chain_id}"
      --rpc-url "${rpc_url}"
      --verifier blockscout
      --verifier-url "${explorer_base%/}/api/"
      --compiler-version "${compiler_version}"
      --num-of-optimizations 200
      --via-ir
      --skip-is-verified-check
      --watch
    )
    if [ -n "${ctor}" ]; then
      args+=(--constructor-args "${ctor}")
    fi
    forge "${args[@]}"
  )
}

proxy_ctor() {
  cast abi-encode "constructor(address,address,bytes)" "$@"
}

token_init_data="$(cast calldata "initialize(string,string,uint8,address)" ZKSYS ZKSYS 18 "${ZKSYS_ADMIN}")"
registry_init_data="$(cast calldata "initialize(address,address)" "${ZKSYS_ADMIN}" 0x0000000000000000000000000000000000000000)"
weight_init_data="$(cast calldata "initialize(address,address,uint256)" "${ZKSYS_ADMIN}" "${REGISTRY_PROXY}" 3)"
issuer_start_time="$(numeric_call "${ZKSYS_L2_RPC_URL}" "${ISSUER_PROXY}" "startTime()(uint256)")"
issuer_init_data="$(cast calldata "initialize(address,address,address,uint256,uint256,uint256)" "${TOKEN_PROXY}" "${WEIGHT_REGISTRY_PROXY}" "${ZKSYS_ADMIN}" "${issuer_start_time}" 86400 365)"
staking_init_data="$(cast calldata "initialize(address)" "${WEIGHT_REGISTRY_PROXY}")"

verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${L2_PROXY_ADMIN}" "zkSYS ProxyAdmin" "src/zksys/ZkSysProxyAdmin.sol:ZkSysProxyAdmin" \
  "$(cast abi-encode "constructor(address)" "${ZKSYS_ADMIN}")"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_TOKEN_SOLC}" \
  "${TOKEN_IMPL}" "zkSYS token implementation" "src/zksys/SyscoinZKSYSToken.sol:SyscoinZKSYSToken"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${TOKEN_PROXY}" "zkSYS token proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${TOKEN_IMPL}" "${L2_PROXY_ADMIN}" "${token_init_data}")"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${REGISTRY_IMPL}" "zkSYS membership registry implementation" "src/zksys/ZkSysMembershipRegistry.sol:ZkSysMembershipRegistry"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${REGISTRY_PROXY}" "zkSYS membership registry proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${REGISTRY_IMPL}" "${L2_PROXY_ADMIN}" "${registry_init_data}")"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${WEIGHT_REGISTRY_IMPL}" "zkSYS reward weight registry implementation" "src/zksys/ZkSysRewardWeightRegistry.sol:ZkSysRewardWeightRegistry"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${WEIGHT_REGISTRY_PROXY}" "zkSYS reward weight registry proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${WEIGHT_REGISTRY_IMPL}" "${L2_PROXY_ADMIN}" "${weight_init_data}")"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${ISSUER_IMPL}" "zkSYS issuer implementation" "src/zksys/ZkSysIssuer.sol:ZkSysIssuer"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${ISSUER_PROXY}" "zkSYS issuer proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${ISSUER_IMPL}" "${L2_PROXY_ADMIN}" "${issuer_init_data}")"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${STAKING_VAULT_IMPL}" "zkSYS native staking vault implementation" "src/zksys/ZkSysNativeStakingVault.sol:ZkSysNativeStakingVault"
verify_contract "${ZKSYS_L2_RPC_URL}" "${ZKSYS_L2_EXPLORER_BASE}" "${ZKSYS_L2_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${STAKING_VAULT_PROXY}" "zkSYS native staking vault proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${STAKING_VAULT_IMPL}" "${L2_PROXY_ADMIN}" "${staking_init_data}")"

if [ -z "${L1_BRIDGEHUB}" ]; then
  L1_BRIDGEHUB="$(cast call --rpc-url "${L1_RPC_URL}" "${L1_BRIDGE_PROXY}" "bridgehub()(address)")"
fi
l1_bridge_init_data="$(
  cast calldata \
    "initialize(address,uint256,address,uint32,uint32,uint32,uint16,uint16)" \
    "${L1_BRIDGEHUB}" \
    "${ZKSYS_L2_CHAIN_ID}" \
    "${REGISTRY_PROXY}" \
    "${L1_BRIDGE_NEVM_START_BLOCK}" \
    "${L1_BRIDGE_SENIORITY_HEIGHT1}" \
    "${L1_BRIDGE_SENIORITY_HEIGHT2}" \
    "${L1_BRIDGE_SENIORITY_LEVEL1_BPS}" \
    "${L1_BRIDGE_SENIORITY_LEVEL2_BPS}"
)"

verify_contract "${L1_RPC_URL}" "${L1_EXPLORER_BASE}" "${L1_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${L1_BRIDGE_PROXY_ADMIN}" "zkSYS L1 registry bridge ProxyAdmin" "src/zksys/ZkSysProxyAdmin.sol:ZkSysProxyAdmin" \
  "$(cast abi-encode "constructor(address)" "${ZKSYS_ADMIN}")"
verify_contract "${L1_RPC_URL}" "${L1_EXPLORER_BASE}" "${L1_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${L1_BRIDGE_IMPL}" "zkSYS L1 registry bridge implementation" "src/zksys/ZkSysRegistryBridge.sol:ZkSysRegistryBridge"
verify_contract "${L1_RPC_URL}" "${L1_EXPLORER_BASE}" "${L1_CHAIN_ID}" "${ZKSYS_SOLC}" \
  "${L1_BRIDGE_PROXY}" "zkSYS L1 registry bridge proxy" "src/zksys/ZkSysCreate2ProxyBytecode.sol:ZkSysCreate2ProxyBytecode" \
  "$(proxy_ctor "${L1_BRIDGE_IMPL}" "${L1_BRIDGE_PROXY_ADMIN}" "${l1_bridge_init_data}")"

echo "zkSYS Blockscout verification submissions complete."
