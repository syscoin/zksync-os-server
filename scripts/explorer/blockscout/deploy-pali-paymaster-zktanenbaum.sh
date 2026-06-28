#!/usr/bin/env bash
# Deploy and verify the Pali fixed-rate zkSYS ERC-4337 paymaster on zkTanenbaum.
#
# Required:
#   ZKSYS_TOKEN_ADDRESS      zkSYS ERC-20 token address on zkTanenbaum
#
# Optional:
#   ZKTANENBAUM_RPC_URL     default: https://rpc-zk.tanenbaum.io
#   EXPLORER_BASE           default: https://explorer-zk.tanenbaum.io
#   ENTRYPOINT_ADDRESS      SyscoinEntryPoint used by the Pali account/factory stack.
#                           The deployed paymaster self-binds to it in its constructor.
#   PAYMASTER_OWNER         default: deployer address, if DEPLOYER_ADDRESS is set
#   DEPLOYER_ADDRESS        used as default owner for hardware or keystore signers
#   DEPLOYER_PRIVATE_KEY    raw private key signer
#   DEPLOYER_MNEMONIC       mnemonic signer, index 0 by default
#   DEPLOYER_MNEMONIC_INDEX mnemonic index, default: 0
#   DEPLOYER_ACCOUNT        Foundry keystore account name
#   DEPLOYER_KEYSTORE       Foundry keystore path
#   DEPLOYER_PASSWORD_FILE  password file for DEPLOYER_KEYSTORE / DEPLOYER_ACCOUNT
#   DEPLOYER_SIGNER         ledger | trezor | aws | gcp
#   PAYMASTER_INITIAL_DEPOSIT_NATIVE
#                           optional native amount to send to paymaster after deployment
#                           (for example: 1000ether)
#   PAYMASTER_TARGET_ENTRYPOINT_RESERVE_NATIVE
#                           EntryPoint deposit cap before excess native is sent to the
#                           Syscoin unspendable sink, default: 100000ether
#   PAYMASTER_STAKE_NATIVE  optional native stake amount to add after deployment
#                           (required by ERC-4337 bundlers for storage-accessing paymasters)
#   PAYMASTER_UNSTAKE_DELAY_SEC
#                           unstake delay for PAYMASTER_STAKE_NATIVE, default: 86400
#   PAYMASTER_GRANT_BURNER_ROLE
#                           true by default; grants zkSYS BURNER_ROLE to the deployed paymaster
#                           using the deployer signer. Set false only if role wiring is handled
#                           separately before the paymaster is used.
#   GATEWAY_DIR             default: ~/gateway; chain config to update after deployment
#   EDGE_CHAIN_NAME         default: zksys; chain config to update after deployment
#   UPDATE_CHAIN_FEE_COLLECTOR
#                           true by default; writes deployed paymaster to
#                           chains/$EDGE_CHAIN_NAME/configs/contracts.yaml
#                           as l2.zksys_fee_collector_addr
#   VERIFY                  true by default; set false to skip Blockscout verification
#
# Example:
#   ZKSYS_TOKEN_ADDRESS=0x... DEPLOYER_PRIVATE_KEY=0x... \
#     ./scripts/explorer/blockscout/deploy-pali-paymaster-zktanenbaum.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/contracts"

SYSCOIN_ENTRYPOINT_ADDRESS="0x4337A33B1992cAaF981Da5aa13ac4D31a5e96F77"
# Standard CREATE2 deployer 0x4e59... with salt
# 0x2936176356794aba724e18c0d2b55c58521f932b8747bc67fadcde259ff2221e.
# The salt is chosen so the Syscoin EntryPoint keeps the upstream-style 0x4337 prefix.

RPC_URL="${ZKTANENBAUM_RPC_URL:-https://rpc-zk.tanenbaum.io}"
EXPLORER_BASE="${EXPLORER_BASE:-https://explorer-zk.tanenbaum.io}"
CHAIN_ID="${CHAIN_ID:-57057}"
ENTRYPOINT_ADDRESS="${ENTRYPOINT_ADDRESS:-${SYSCOIN_ENTRYPOINT_ADDRESS}}"
VERIFY="${VERIFY:-true}"
PAYMASTER_INITIAL_DEPOSIT_NATIVE="${PAYMASTER_INITIAL_DEPOSIT_NATIVE:-}"
PAYMASTER_TARGET_ENTRYPOINT_RESERVE_NATIVE="${PAYMASTER_TARGET_ENTRYPOINT_RESERVE_NATIVE:-100000ether}"
PAYMASTER_STAKE_NATIVE="${PAYMASTER_STAKE_NATIVE:-}"
PAYMASTER_UNSTAKE_DELAY_SEC="${PAYMASTER_UNSTAKE_DELAY_SEC:-86400}"
PAYMASTER_GRANT_BURNER_ROLE="${PAYMASTER_GRANT_BURNER_ROLE:-true}"
GATEWAY_DIR="${GATEWAY_DIR:-${HOME}/gateway}"
EDGE_CHAIN_NAME="${EDGE_CHAIN_NAME:-zksys}"
UPDATE_CHAIN_FEE_COLLECTOR="${UPDATE_CHAIN_FEE_COLLECTOR:-true}"

if [[ -z "${ZKSYS_TOKEN_ADDRESS:-}" ]]; then
  echo "error: ZKSYS_TOKEN_ADDRESS is required" >&2
  exit 1
fi

if [[ -z "${DEPLOYER_ADDRESS:-}" && -n "${DEPLOYER_PRIVATE_KEY:-}" ]]; then
  DEPLOYER_ADDRESS="$(cast wallet address --private-key "${DEPLOYER_PRIVATE_KEY}")"
fi

PAYMASTER_OWNER="${PAYMASTER_OWNER:-${DEPLOYER_ADDRESS:-}}"
if [[ -z "${PAYMASTER_OWNER}" ]]; then
  echo "error: set PAYMASTER_OWNER, or set DEPLOYER_ADDRESS as its default" >&2
  exit 1
fi
if [[ -z "${ENTRYPOINT_ADDRESS}" ]]; then
  echo "error: ENTRYPOINT_ADDRESS is required and must be the SyscoinEntryPoint used by the Pali account/factory stack" >&2
  exit 1
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

if [[ "${PAYMASTER_GRANT_BURNER_ROLE}" != "true" && "${PAYMASTER_GRANT_BURNER_ROLE}" != "false" ]]; then
  echo "error: PAYMASTER_GRANT_BURNER_ROLE must be true or false" >&2
  exit 1
fi
if [[ "${UPDATE_CHAIN_FEE_COLLECTOR}" != "true" && "${UPDATE_CHAIN_FEE_COLLECTOR}" != "false" ]]; then
  echo "error: UPDATE_CHAIN_FEE_COLLECTOR must be true or false" >&2
  exit 1
fi

verify_args=()
if [[ "${VERIFY}" == "true" ]]; then
  verify_args+=(--verify --verifier blockscout --verifier-url "${EXPLORER_BASE%/}/api/")
fi

entrypoint_sponsored_paymaster="$(cast call "${ENTRYPOINT_ADDRESS}" \
  "SYSCOIN_SPONSORED_PAYMASTER()(address)" \
  --rpc-url "${RPC_URL}")"
if [[ "$(printf '%s' "${entrypoint_sponsored_paymaster}" | tr '[:upper:]' '[:lower:]')" != "0x0000000000000000000000000000000000000000" ]]; then
  echo "error: SyscoinEntryPoint ${ENTRYPOINT_ADDRESS} is already bound to ${entrypoint_sponsored_paymaster}" >&2
  exit 1
fi

echo "Deploying PaliFixedRateTokenPaymaster"
echo "  rpc:       ${RPC_URL}"
echo "  chain:     ${CHAIN_ID}"
echo "  entrypoint:${ENTRYPOINT_ADDRESS}"
echo "  token:     ${ZKSYS_TOKEN_ADDRESS}"
echo "  owner:     ${PAYMASTER_OWNER}"
echo "  reserve:   ${PAYMASTER_TARGET_ENTRYPOINT_RESERVE_NATIVE}"
echo

output="$(
  cd "${CONTRACTS_DIR}"
  forge create src/pali/PaliFixedRateTokenPaymaster.sol:PaliFixedRateTokenPaymaster \
    --rpc-url "${RPC_URL}" \
    --chain "${CHAIN_ID}" \
    --broadcast \
    --optimize \
    --optimizer-runs 200 \
    "${verify_args[@]}" \
    "${wallet_args[@]}" \
    --constructor-args \
      "${ENTRYPOINT_ADDRESS}" \
      "${ZKSYS_TOKEN_ADDRESS}" \
      "${PAYMASTER_OWNER}" \
      "${PAYMASTER_TARGET_ENTRYPOINT_RESERVE_NATIVE}"
)"

printf '%s\n' "${output}"

paymaster_address="$(printf '%s\n' "${output}" | sed -n 's/^Deployed to: //p' | tail -n 1)"
if [[ -n "${paymaster_address}" ]]; then
  echo
  echo "PAYMASTER_ADDRESS=${paymaster_address}"

  paymaster_entrypoint="$(cast call "${paymaster_address}" "entryPoint()(address)" --rpc-url "${RPC_URL}")"
  if [[ "$(printf '%s' "${paymaster_entrypoint}" | tr '[:upper:]' '[:lower:]')" != "$(printf '%s' "${ENTRYPOINT_ADDRESS}" | tr '[:upper:]' '[:lower:]')" ]]; then
    echo "error: deployed paymaster entryPoint()=${paymaster_entrypoint}, expected ${ENTRYPOINT_ADDRESS}" >&2
    exit 1
  fi

  entrypoint_sponsored_paymaster="$(cast call "${ENTRYPOINT_ADDRESS}" \
    "SYSCOIN_SPONSORED_PAYMASTER()(address)" \
    --rpc-url "${RPC_URL}")"
  if [[ "$(printf '%s' "${entrypoint_sponsored_paymaster}" | tr '[:upper:]' '[:lower:]')" != "$(printf '%s' "${paymaster_address}" | tr '[:upper:]' '[:lower:]')" ]]; then
    echo "error: SyscoinEntryPoint ${ENTRYPOINT_ADDRESS} sponsors ${entrypoint_sponsored_paymaster}, expected ${paymaster_address}" >&2
    exit 1
  fi

  burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "BURNER_ROLE()(bytes32)" --rpc-url "${RPC_URL}")"
  has_burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "${burner_role}" "${paymaster_address}" --rpc-url "${RPC_URL}")"
  if [[ "${has_burner_role}" != "true" ]]; then
    if [[ "${PAYMASTER_GRANT_BURNER_ROLE}" == "true" ]]; then
      echo
      echo "Granting zkSYS BURNER_ROLE to paymaster ${paymaster_address}"
      cast send "${ZKSYS_TOKEN_ADDRESS}" \
        "grantRole(bytes32,address)" "${burner_role}" "${paymaster_address}" \
        --rpc-url "${RPC_URL}" \
        --chain "${CHAIN_ID}" \
        "${wallet_args[@]}"
    else
      echo
      echo "warning: paymaster ${paymaster_address} does not have zkSYS BURNER_ROLE; postOp burns will fail until the role is granted." >&2
    fi
  fi

  has_burner_role="$(cast call "${ZKSYS_TOKEN_ADDRESS}" "hasRole(bytes32,address)(bool)" "${burner_role}" "${paymaster_address}" --rpc-url "${RPC_URL}")"

  if [[ -n "${PAYMASTER_INITIAL_DEPOSIT_NATIVE}" ]]; then
    echo
    echo "Depositing ${PAYMASTER_INITIAL_DEPOSIT_NATIVE} native into EntryPoint for paymaster ${paymaster_address}"
    cast send "${paymaster_address}" \
      --rpc-url "${RPC_URL}" \
      --chain "${CHAIN_ID}" \
      --value "${PAYMASTER_INITIAL_DEPOSIT_NATIVE}" \
      "${wallet_args[@]}"
  fi

  if [[ -n "${PAYMASTER_STAKE_NATIVE}" ]]; then
    echo
    echo "Staking ${PAYMASTER_STAKE_NATIVE} native for paymaster ${paymaster_address}"
    cast send "${paymaster_address}" \
      "addStake(uint32)" "${PAYMASTER_UNSTAKE_DELAY_SEC}" \
      --rpc-url "${RPC_URL}" \
      --chain "${CHAIN_ID}" \
      --value "${PAYMASTER_STAKE_NATIVE}" \
      "${wallet_args[@]}"
  else
    echo
    echo "warning: PAYMASTER_STAKE_NATIVE was not set; ERC-4337 bundlers may reject this storage-accessing paymaster until addStake is called." >&2
  fi

  if [[ "${UPDATE_CHAIN_FEE_COLLECTOR}" == "true" ]]; then
    if [[ "${has_burner_role}" != "true" ]]; then
      echo "error: refusing to write zksys fee collector; paymaster ${paymaster_address} lacks zkSYS BURNER_ROLE" >&2
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
    python3 - "${contracts_yaml}" "${paymaster_address}" <<'PY'
import re
import sys
from pathlib import Path

import yaml

path = Path(sys.argv[1])
address = sys.argv[2].strip().lower()
if not re.fullmatch(r"0x[0-9a-f]{40}", address) or address == "0x" + "0" * 40:
    raise SystemExit("paymaster address must be a nonzero 20-byte hex address")

data = yaml.safe_load(path.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit(f"invalid YAML object in {path}")
l2 = data.setdefault("l2", {})
if not isinstance(l2, dict):
    raise SystemExit(f"invalid l2 section in {path}")
l2["zksys_fee_collector_addr"] = address
path.write_text(yaml.safe_dump(data, sort_keys=False, allow_unicode=True), encoding="utf-8")
PY
    echo "Updated ${contracts_yaml}: l2.zksys_fee_collector_addr=${paymaster_address}"
  fi
fi
