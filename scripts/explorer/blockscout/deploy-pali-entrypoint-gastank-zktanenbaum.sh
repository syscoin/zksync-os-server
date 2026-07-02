#!/usr/bin/env bash
# Deploy the standard ERC-4337 EntryPoint v0.9 (canonical global address) and
# the ZkSysGasTank on zkTanenbaum.
#
# zkSYS gas payment is native to the chain: the patched ZKsync OS bootloader
# debits prepaid zkSYS from the gas tank 1:1 during fee charging. There is no
# custom EntryPoint and no paymaster; the account/factory stack targets the
# canonical EntryPoint v0.9 singleton, deployed deterministically so the
# address matches every other chain.
#
# Required:
#   ZKSYS_TOKEN_ADDRESS      zkSYS ERC-20 token address on zkTanenbaum
#
# Optional:
#   ZKTANENBAUM_RPC_URL     default: https://rpc-zk.tanenbaum.io
#   EXPLORER_BASE           default: https://explorer-zk.tanenbaum.io
#   DEPLOYER_ADDRESS        used with hardware or keystore signers
#   DEPLOYER_PRIVATE_KEY    raw private key signer
#   DEPLOYER_MNEMONIC       mnemonic signer, index 0 by default
#   DEPLOYER_MNEMONIC_INDEX mnemonic index, default: 0
#   DEPLOYER_ACCOUNT        Foundry keystore account name
#   DEPLOYER_KEYSTORE       Foundry keystore path
#   DEPLOYER_PASSWORD_FILE  password file for DEPLOYER_KEYSTORE / DEPLOYER_ACCOUNT
#   DEPLOYER_SIGNER         ledger | trezor | aws | gcp
#   ENTRYPOINT_INIT_CODE_FILE
#                           file with the official EntryPoint v0.9 creation
#                           bytecode (hex). Default: built from the vendored
#                           account-abstraction v0.9.0 checkout with the
#                           official compiler settings. Either way the CREATE2
#                           address is asserted against the canonical
#                           EntryPoint address before any deployment.
#   GAS_TANK_GRANT_BURNER_ROLE
#                           true by default; grants zkSYS BURNER_ROLE to the
#                           deployed gas tank so burnSurplus() works. Set
#                           false only if role wiring is handled separately.
#   GATEWAY_DIR             default: ~/gateway; chain config to update after deployment
#   EDGE_CHAIN_NAME         default: zksys; chain config to update after deployment
#   UPDATE_CHAIN_GAS_TANK   true by default; writes deployed gas tank to
#                           chains/$EDGE_CHAIN_NAME/configs/contracts.yaml
#                           as l2.zksys_gas_tank_addr (consumed when baking
#                           the patched ZKsync OS via SYSCOIN_GAS_TANK_ADDRESS)
#   VERIFY                  true by default; set false to skip Blockscout verification
#
# Example:
#   ZKSYS_TOKEN_ADDRESS=0x... DEPLOYER_PRIVATE_KEY=0x... \
#     ./scripts/explorer/blockscout/deploy-pali-entrypoint-gastank-zktanenbaum.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"
AA_DIR="${REPO_ROOT}/integration-tests/test-contracts/lib/account-abstraction"

# Canonical ERC-4337 EntryPoint v0.9 singleton
# (eth-infinitism/account-abstraction v0.9.0 release).
CANONICAL_ENTRYPOINT_V09="0x433709009B8330FDa32311DF1C2AFA402eD8D009"
CREATE2_DEPLOYER_ADDRESS="0x4e59b44847b379578588920cA78FbF26c0B4956C"
# Official deployment salt from the account-abstraction v0.9.0 release
# (hardhat.config.ts SALT default).
ENTRYPOINT_V09_SALT="0x7702864008ddeab30aa67b7adc3d2653bc8d162714b1fe8fe4582df814f3bf61"

RPC_URL="${ZKTANENBAUM_RPC_URL:-https://rpc-zk.tanenbaum.io}"
EXPLORER_BASE="${EXPLORER_BASE:-https://explorer-zk.tanenbaum.io}"
CHAIN_ID="${CHAIN_ID:-57057}"
VERIFY="${VERIFY:-true}"
GAS_TANK_GRANT_BURNER_ROLE="${GAS_TANK_GRANT_BURNER_ROLE:-true}"
GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
UPDATE_CHAIN_GAS_TANK="${UPDATE_CHAIN_GAS_TANK:-true}"

if [[ -z "${ZKSYS_TOKEN_ADDRESS:-}" ]]; then
  echo "error: ZKSYS_TOKEN_ADDRESS is required" >&2
  exit 1
fi

if [[ -z "${DEPLOYER_ADDRESS:-}" && -n "${DEPLOYER_PRIVATE_KEY:-}" ]]; then
  DEPLOYER_ADDRESS="$(cast wallet address --private-key "${DEPLOYER_PRIVATE_KEY}")"
fi

wallet_args=()
case "${DEPLOYER_SIGNER:-}" in
  "")
    if [[ -n "${DEPLOYER_PRIVATE_KEY:-}" ]]; then
      wallet_args+=(--private-key "${DEPLOYER_PRIVATE_KEY}")
    elif [[ -n "${DEPLOYER_MNEMONIC:-}" ]]; then
      wallet_args+=(--mnemonic "${DEPLOYER_MNEMONIC}" --mnemonic-index "${DEPLOYER_MNEMONIC_INDEX:-0}")
    elif [[ -n "${DEPLOYER_ACCOUNT:-}" ]]; then
      wallet_args+=(--account "${DEPLOYER_ACCOUNT}")
    elif [[ -n "${DEPLOYER_KEYSTORE:-}" ]]; then
      wallet_args+=(--keystore "${DEPLOYER_KEYSTORE}")
    else
      echo "error: set DEPLOYER_PRIVATE_KEY, DEPLOYER_ACCOUNT, DEPLOYER_KEYSTORE, or DEPLOYER_SIGNER" >&2
      exit 1
    fi
    ;;
  ledger) wallet_args+=(--ledger --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for ledger}") ;;
  trezor) wallet_args+=(--trezor --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for trezor}") ;;
  aws) wallet_args+=(--aws --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for aws}") ;;
  gcp) wallet_args+=(--gcp --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for gcp}") ;;
  *)
    echo "error: unsupported DEPLOYER_SIGNER=${DEPLOYER_SIGNER}" >&2
    exit 1
    ;;
esac

if [[ -n "${DEPLOYER_PASSWORD_FILE:-}" ]]; then
  wallet_args+=(--password-file "${DEPLOYER_PASSWORD_FILE}")
fi

if [[ "${GAS_TANK_GRANT_BURNER_ROLE}" != "true" && "${GAS_TANK_GRANT_BURNER_ROLE}" != "false" ]]; then
  echo "error: GAS_TANK_GRANT_BURNER_ROLE must be true or false" >&2
  exit 1
fi
if [[ "${UPDATE_CHAIN_GAS_TANK}" != "true" && "${UPDATE_CHAIN_GAS_TANK}" != "false" ]]; then
  echo "error: UPDATE_CHAIN_GAS_TANK must be true or false" >&2
  exit 1
fi

verify_args=()
if [[ "${VERIFY}" == "true" ]]; then
  verify_args+=(--verify --verifier blockscout --verifier-url "${EXPLORER_BASE%/}/api/")
fi

lower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

rpc_code() {
  cast code --rpc-url "${RPC_URL}" "${1:?address required}"
}

# ---------------------------------------------------------------------------
# 1. Standard EntryPoint v0.9 at the canonical global address.
#
# Atomic by construction: the EntryPoint singleton has no owner, no
# initializer and no post-deploy wiring. The CREATE2 address is asserted
# against the canonical address before deploying, which also proves the init
# code is byte-identical to the official release.
# ---------------------------------------------------------------------------
entrypoint_init_code() {
  if [[ -n "${ENTRYPOINT_INIT_CODE_FILE:-}" ]]; then
    tr -d '[:space:]' < "${ENTRYPOINT_INIT_CODE_FILE}"
    return
  fi
  # Build from the vendored v0.9.0 checkout with the official compiler
  # settings (solc 0.8.28, optimizer runs 1,000,000, via-ir, default
  # ipfs metadata hash).
  (
    cd "${AA_DIR}"
    FOUNDRY_PROFILE=default forge inspect \
      --use 0.8.28 --no-auto-detect \
      --optimize --optimizer-runs 1000000 --via-ir \
      contracts/core/EntryPoint.sol:EntryPoint bytecode
  )
}

deploy_canonical_entrypoint_if_missing() {
  local create2_code entrypoint_code init_code computed

  entrypoint_code="$(rpc_code "${CANONICAL_ENTRYPOINT_V09}")"
  if [[ "${entrypoint_code}" != "0x" ]]; then
    echo "EntryPoint v0.9 already deployed at ${CANONICAL_ENTRYPOINT_V09}"
    return
  fi

  create2_code="$(rpc_code "${CREATE2_DEPLOYER_ADDRESS}")"
  if [[ "${create2_code}" == "0x" ]]; then
    echo "error: canonical CREATE2 deployer has no code at ${CREATE2_DEPLOYER_ADDRESS}" >&2
    exit 1
  fi

  init_code="$(entrypoint_init_code)"
  init_code="0x${init_code#0x}"
  computed="$(
    cast create2 \
      --deployer "${CREATE2_DEPLOYER_ADDRESS}" \
      --salt "${ENTRYPOINT_V09_SALT}" \
      --init-code "${init_code}"
  )"
  if [[ "$(lower "${computed}")" != "$(lower "${CANONICAL_ENTRYPOINT_V09}")" ]]; then
    echo "error: EntryPoint CREATE2 address ${computed} does not match the canonical" >&2
    echo "       EntryPoint v0.9 address ${CANONICAL_ENTRYPOINT_V09}." >&2
    echo "       The locally built init code is not byte-identical to the official" >&2
    echo "       release; provide ENTRYPOINT_INIT_CODE_FILE with the official" >&2
    echo "       creation bytecode from eth-infinitism/account-abstraction v0.9.0." >&2
    exit 1
  fi

  echo "Deploying canonical EntryPoint v0.9"
  echo "  address:  ${CANONICAL_ENTRYPOINT_V09}"
  echo "  create2:  ${CREATE2_DEPLOYER_ADDRESS}"
  echo "  salt:     ${ENTRYPOINT_V09_SALT}"
  cast send "${CREATE2_DEPLOYER_ADDRESS}" \
    "${ENTRYPOINT_V09_SALT}${init_code#0x}" \
    --rpc-url "${RPC_URL}" \
    --chain "${CHAIN_ID}" \
    "${wallet_args[@]}" >/dev/null

  entrypoint_code="$(rpc_code "${CANONICAL_ENTRYPOINT_V09}")"
  if [[ "${entrypoint_code}" == "0x" ]]; then
    echo "error: EntryPoint deployment did not create code at ${CANONICAL_ENTRYPOINT_V09}" >&2
    exit 1
  fi
}

deploy_canonical_entrypoint_if_missing

# ---------------------------------------------------------------------------
# 2. ZkSysGasTank.
#
# Atomic by construction: the constructor pins the token (and checks its
# decimals); there is no initializer, owner, or proxy. The only post-deploy
# step is granting BURNER_ROLE on the token so burnSurplus() can destroy the
# base-fee surplus.
# ---------------------------------------------------------------------------
echo
echo "Deploying ZkSysGasTank"
echo "  rpc:    ${RPC_URL}"
echo "  chain:  ${CHAIN_ID}"
echo "  token:  ${ZKSYS_TOKEN_ADDRESS}"
echo

output="$(
  cd "${CONTRACTS_DIR}"
  forge create src/zksys/ZkSysGasTank.sol:ZkSysGasTank \
    --rpc-url "${RPC_URL}" \
    --chain "${CHAIN_ID}" \
    --broadcast \
    "${verify_args[@]}" \
    "${wallet_args[@]}" \
    --constructor-args "${ZKSYS_TOKEN_ADDRESS}"
)"

printf '%s\n' "${output}"

gas_tank_address="$(printf '%s\n' "${output}" | sed -n 's/^Deployed to: //p' | tail -n 1)"
if [[ -z "${gas_tank_address}" ]]; then
  echo "error: could not parse deployed ZkSysGasTank address" >&2
  exit 1
fi

echo
echo "GAS_TANK_ADDRESS=${gas_tank_address}"

tank_token="$(cast call "${gas_tank_address}" "token()(address)" --rpc-url "${RPC_URL}")"
if [[ "$(lower "${tank_token}")" != "$(lower "${ZKSYS_TOKEN_ADDRESS}")" ]]; then
  echo "error: deployed gas tank token()=${tank_token}, expected ${ZKSYS_TOKEN_ADDRESS}" >&2
  exit 1
fi

burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "BURNER_ROLE()(bytes32)" --rpc-url "${RPC_URL}")"
has_burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "${burner_role}" "${gas_tank_address}" --rpc-url "${RPC_URL}")"
if [[ "${has_burner_role}" != "true" ]]; then
  if [[ "${GAS_TANK_GRANT_BURNER_ROLE}" == "true" ]]; then
    echo
    echo "Granting zkSYS BURNER_ROLE to gas tank ${gas_tank_address}"
    cast send "${ZKSYS_TOKEN_ADDRESS}" \
      "grantRole(bytes32,address)" "${burner_role}" "${gas_tank_address}" \
      --rpc-url "${RPC_URL}" \
      --chain "${CHAIN_ID}" \
      "${wallet_args[@]}"
  else
    echo
    echo "warning: gas tank ${gas_tank_address} does not have zkSYS BURNER_ROLE; burnSurplus() will revert until the role is granted." >&2
  fi
fi

has_burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "${burner_role}" "${gas_tank_address}" --rpc-url "${RPC_URL}")"

if [[ "${UPDATE_CHAIN_GAS_TANK}" == "true" ]]; then
  if [[ "${has_burner_role}" != "true" ]]; then
    echo "error: refusing to write l2.zksys_gas_tank_addr; gas tank ${gas_tank_address} lacks zkSYS BURNER_ROLE" >&2
    exit 1
  fi

  contracts_yaml="${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/contracts.yaml"
  if [[ ! -f "${contracts_yaml}" ]]; then
    contracts_yaml="${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/contracts_${CHAIN_ID}.yaml"
  fi
  if [[ ! -f "${contracts_yaml}" ]]; then
    echo "error: missing chain contracts file: ${GATEWAY_DIR}/chains/${EDGE_CHAIN_NAME}/configs/contracts.yaml or contracts_${CHAIN_ID}.yaml" >&2
    exit 1
  fi
  python3 - "${contracts_yaml}" "${gas_tank_address}" <<'PY'
import re
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
address = sys.argv[2].strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", address) or address == "0x" + "0" * 40:
    raise SystemExit("gas tank address must be a nonzero 20-byte hex address")
if int(address[2:], 16) < 1 << 16:
    raise SystemExit("gas tank address must not be in the reserved system address space")

data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {path}")
l2 = data.setdefault("l2", {})
if not isinstance(l2, dict):
    raise SystemExit(f"invalid l2 section in {path}")
l2["zksys_gas_tank_addr"] = address
path.write_text(yaml.safe_dump(data, sort_keys=False, allow_unicode=True), encoding="utf-8")
PY
  echo "Updated ${contracts_yaml}: l2.zksys_gas_tank_addr=${gas_tank_address}"
  echo
  echo "NOTE: the patched ZKsync OS must be (re)built with SYSCOIN_GAS_TANK_ADDRESS=${gas_tank_address}"
  echo "      so the baked constant and the registered VK match the deployed tank."
fi

echo
echo "Done."
echo "  EntryPoint v0.9: ${CANONICAL_ENTRYPOINT_V09}"
echo "  ZkSysGasTank:    ${gas_tank_address}"
